#include "Dx12Output.h"
#include "HmdOutput.h"
#include <stdexcept>
#include <cstdio>
#include <vector>

namespace action75 {
namespace {
void checked(HRESULT hr, const char *op) {
  if (FAILED(hr)) {
    char message[160];
    snprintf(message, sizeof(message), "%s (HRESULT 0x%08lx)", op, static_cast<unsigned long>(hr));
    throw std::runtime_error(message);
  }
}
void waitFor(ID3D12Fence *fence, UINT64 value, HANDLE event) {
  auto completed = fence->GetCompletedValue();
  if (completed == UINT64_MAX)
    throw std::runtime_error("D3D12 device removed");
  if (completed >= value)
    return;
  ResetEvent(event);
  checked(fence->SetEventOnCompletion(value, event), "Arm D3D12 fence");
  const auto deadline = GetTickCount64() + 1000;
  for (auto now = GetTickCount64(); now < deadline; now = GetTickCount64()) {
    if (WaitForSingleObject(event, static_cast<DWORD>(deadline - now)) != WAIT_OBJECT_0)
      break;
    completed = fence->GetCompletedValue();
    if (completed == UINT64_MAX)
      throw std::runtime_error("D3D12 device removed");
    if (completed >= value)
      return;
    // A registration from an earlier timed-out wait can still signal this event.
  }
  throw std::runtime_error("D3D12 frame did not complete");
}
D3D12_HEAP_PROPERTIES heap(D3D12_HEAP_TYPE type) {
  D3D12_HEAP_PROPERTIES p{};
  p.Type = type;
  p.CreationNodeMask = p.VisibleNodeMask = 1;
  return p;
}
void transition(ID3D12GraphicsCommandList *list, ID3D12Resource *resource,
                D3D12_RESOURCE_STATES before, D3D12_RESOURCE_STATES after) {
  D3D12_RESOURCE_BARRIER b{};
  b.Type = D3D12_RESOURCE_BARRIER_TYPE_TRANSITION;
  b.Transition = {resource, D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES, before, after};
  list->ResourceBarrier(1, &b);
}
} // namespace

Dx12CompositionSurface::~Dx12CompositionSurface() {
  content_.Reset();
  if (handle_)
    CloseHandle(handle_);
}

struct Dx12Output::Readback {
  Ptr<ID3D12Device> device;
  Ptr<ID3D12CommandQueue> queue;
  Ptr<ID3D12CommandAllocator> allocator;
  Ptr<ID3D12GraphicsCommandList> commands;
  Ptr<ID3D12Fence> fence;
  Ptr<ID3D12Resource> source, pixels;
  UINT64 value = 0;
  PTP_WORK work = nullptr;

  ~Readback() {
    if (work)
      CloseThreadpoolWork(work);
  }
  bool complete() const noexcept {
    return fence->GetCompletedValue() >= value || FAILED(device->GetDeviceRemovedReason());
  }
  static void CALLBACK retire(PTP_CALLBACK_INSTANCE callback, void *context, PTP_WORK) {
    std::unique_ptr<Readback> readback(static_cast<Readback *>(context));
    CallbackMayRunLong(callback);
    if (!readback->complete())
      readback->fence->SetEventOnCompletion(readback->value, nullptr);
    // Registration can fail under memory pressure. The commands still own
    // these resources until the fence completes or the device is removed.
    while (!readback->complete())
      Sleep(100);
  }
};

Dx12Output::Dx12Output() = default;
bool Dx12Output::initializeHmd(const std::wstring &path, std::string &error) {
  hmd_ = HmdOutput::open(path, error);
  if (!hmd_) return false;
  event_ = CreateEventW(nullptr, FALSE, FALSE, nullptr);
  if (!event_) {
    error = "Create HMD worker validation event failed";
    return false;
  }
  device_ = hmd_->device();
  width = HmdOutput::Width;
  height = HmdOutput::Height;
  const auto luid = hmd_->adapterLuid();
  adapterLuid = (uint64_t(uint32_t(luid.HighPart)) << 32) | luid.LowPart;
  adapterName = L"Action75 HMD";
  return true;
}
Dx12Output::~Dx12Output() {
  clearComposition();
  if (readback_ && !readback_->complete()) {
    const auto work = readback_.release()->work;
    SubmitThreadpoolWork(work);
  } else if (completion_ && submitted_ && event_) {
    try {
      waitFor(completion_.Get(), submitted_, event_);
    } catch (...) {
    }
  }
  if (frameReady)
    CloseHandle(frameReady);
  if (event_)
    CloseHandle(event_);
  if (removedEvent)
    CloseHandle(removedEvent);
}
bool Dx12Output::initializeComposition(HWND window, unsigned w, unsigned h, GpuPreference preference,
                                       std::string &error) {
  try {
    Ptr<IDXGIFactory6> factory;
    checked(CreateDXGIFactory1(IID_PPV_ARGS(&factory)), "Create DXGI factory");
    const auto order = preference == GpuPreference::Discrete ? DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE
                                                              : DXGI_GPU_PREFERENCE_MINIMUM_POWER;
    Ptr<IDXGIAdapter1> adapter;
    DXGI_ADAPTER_DESC1 desc{};
    for (UINT index = 0;; ++index) {
      checked(factory->EnumAdapterByGpuPreference(index, order,
                                                   IID_PPV_ARGS(adapter.ReleaseAndGetAddressOf())),
              "Select composition adapter");
      checked(adapter->GetDesc1(&desc), "Read composition adapter");
      if (!(desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE))
        break;
    }
    // The compositor binds the player HWND. Only the worker creates a D3D12 device.
    checked(DCompositionCreateDevice2(nullptr, IID_PPV_ARGS(&composition_)), "Create desktop compositor");
    if (!retargetComposition(window, error))
      return false;
    compositionAdapter_ = desc.AdapterLuid;
    adapterName = desc.Description;
    adapterLuid = (uint64_t(uint32_t(desc.AdapterLuid.HighPart)) << 32) | desc.AdapterLuid.LowPart;
    return resize(w, h, error);
  } catch (const std::exception &failure) {
    error = failure.what();
    return false;
  }
}

bool Dx12Output::retargetComposition(HWND window, std::string &error) {
  if (!composition_ || !IsWindow(window)) {
    error = "Invalid wallpaper composition window";
    return false;
  }
  if (compositionTarget_ && compositionWindow_ == window)
    return true;
  try {
    Ptr<IDCompositionTarget> target;
    Ptr<IDCompositionVisual2> visual;
    checked(composition_->CreateTargetForHwnd(window, TRUE, &target), "Bind wallpaper window");
    checked(composition_->CreateVisual(&visual), "Create wallpaper visual");
    if (shownSurface_)
      checked(visual->SetContent(shownSurface_->content_.Get()), "Attach current wallpaper surface");
    checked(target->SetRoot(visual.Get()), "Set wallpaper visual");
    // Commit the old detach and new attach together. An Explorer restart may have
    // already destroyed the previous destination, in which case it needs no detach.
    const bool detach = compositionTarget_ && IsWindow(compositionWindow_);
    HRESULT hr = detach ? compositionTarget_->SetRoot(nullptr) : S_OK;
    if (SUCCEEDED(hr))
      hr = composition_->Commit();
    if (FAILED(hr)) {
      target->SetRoot(nullptr);
      if (detach)
        compositionTarget_->SetRoot(visual_.Get());
      composition_->Commit();
      checked(hr, "Move wallpaper visual");
    }
    compositionTarget_ = std::move(target);
    visual_ = std::move(visual);
    compositionWindow_ = window;
    return true;
  } catch (const std::exception &failure) {
    error = failure.what();
    return false;
  }
}

std::shared_ptr<Dx12CompositionSurface> Dx12Output::makeCompositionSurface(std::string &error) {
  try {
    auto surface = std::make_shared<Dx12CompositionSurface>();
    constexpr DWORD compositionReadWrite = 0x0003;
    checked(DCompositionCreateSurfaceHandle(compositionReadWrite, nullptr, &surface->handle_),
            "Create wallpaper composition surface");
    checked(composition_->CreateSurfaceFromHandle(surface->handle_, &surface->content_),
            "Open wallpaper composition surface");
    return surface;
  } catch (const std::exception &failure) {
    error = failure.what();
    return {};
  }
}

bool Dx12Output::showComposition(const std::shared_ptr<Dx12CompositionSurface> &surface,
                                 std::string &error) {
  if (validating)
    return true;
  try {
    if (surface != shownSurface_) {
      checked(visual_->SetContent(surface->content_.Get()), "Set wallpaper surface");
      const auto committed = composition_->Commit();
      if (FAILED(committed)) {
        visual_->SetContent(shownSurface_ ? shownSurface_->content_.Get() : nullptr);
        composition_->Commit();
        checked(committed, "Commit wallpaper surface");
      }
      shownSurface_ = surface;
    }
    return true;
  } catch (const std::exception &failure) {
    error = failure.what();
    return false;
  }
}

void Dx12Output::clearComposition() noexcept {
  if (visual_ && shownSurface_) {
    visual_->SetContent(nullptr);
    composition_->Commit();
    shownSurface_.reset();
  }
  compositionPresent_ = S_OK;
}

bool Dx12Output::initialize(HWND window, unsigned w, unsigned h, bool warp,
                            GpuPreference preference, std::string &error) {
  try {
    Ptr<IDXGIFactory6> factory;
    checked(CreateDXGIFactory1(IID_PPV_ARGS(&factory)), "Create DXGI factory");
    Ptr<IDXGIAdapter1> adapter;
    if (warp) {
      checked(factory->EnumWarpAdapter(IID_PPV_ARGS(&adapter)), "Select D3D12 WARP adapter");
      checked(D3D12CreateDevice(adapter.Get(), D3D_FEATURE_LEVEL_11_0, IID_PPV_ARGS(&device_)),
              "Create D3D12 WARP device");
    } else {
      auto order = preference == GpuPreference::Discrete ? DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE
                                                         : DXGI_GPU_PREFERENCE_MINIMUM_POWER;
      for (UINT i = 0; SUCCEEDED(factory->EnumAdapterByGpuPreference(
               i, order, IID_PPV_ARGS(adapter.ReleaseAndGetAddressOf())));
           ++i) {
        DXGI_ADAPTER_DESC1 d{};
        adapter->GetDesc1(&d);
        if (!(d.Flags & DXGI_ADAPTER_FLAG_SOFTWARE) &&
            SUCCEEDED(
                D3D12CreateDevice(adapter.Get(), D3D_FEATURE_LEVEL_11_0, IID_PPV_ARGS(&device_))))
          break;
      }
      if (!device_)
        throw std::runtime_error("No compatible Direct3D 12 graphics adapter");
    }
    DXGI_ADAPTER_DESC1 desc{};
    checked(adapter->GetDesc1(&desc), "Read DX12 adapter");
    adapterName = desc.Description;
    adapterLuid = (uint64_t(uint32_t(desc.AdapterLuid.HighPart)) << 32) | desc.AdapterLuid.LowPart;
    D3D12_COMMAND_QUEUE_DESC queueDesc{};
    checked(device_->CreateCommandQueue(&queueDesc, IID_PPV_ARGS(&queue_)), "Create output queue");
    checked(
        device_->CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT, IID_PPV_ARGS(&allocator_)),
        "Create output allocator");
    checked(device_->CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, allocator_.Get(), nullptr,
                                       IID_PPV_ARGS(&list_)),
            "Create output command list");
    checked(list_->Close(), "Close output command list");
    checked(device_->CreateFence(0, D3D12_FENCE_FLAG_NONE, IID_PPV_ARGS(&completion_)),
            "Create output completion fence");
    event_ = CreateEventW(nullptr, FALSE, FALSE, nullptr);
    removedEvent = CreateEventW(nullptr, TRUE, FALSE, nullptr);
    if (!event_ || !removedEvent)
      throw std::runtime_error("Cannot create output fence events");
    checked(completion_->SetEventOnCompletion(UINT64_MAX, removedEvent),
            "Arm D3D12 removal notification");
    if (window) {
      DXGI_SWAP_CHAIN_DESC1 d{};
      d.Width = w;
      d.Height = h;
      d.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
      d.SampleDesc.Count = 1;
      d.BufferUsage = DXGI_USAGE_RENDER_TARGET_OUTPUT;
      d.BufferCount = 2;
      d.SwapEffect = DXGI_SWAP_EFFECT_FLIP_DISCARD;
      d.Flags = DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT;
      Ptr<IDXGISwapChain1> base;
      checked(factory->CreateSwapChainForHwnd(queue_.Get(), window, &d, nullptr, nullptr, &base),
              "Create D3D12 swap chain");
      checked(base.As(&chain_), "Get D3D12 swap chain");
      factory->MakeWindowAssociation(window, DXGI_MWA_NO_ALT_ENTER);
      checked(chain_->SetMaximumFrameLatency(1), "Set D3D12 frame latency");
      frameReady = chain_->GetFrameLatencyWaitableObject();
      if (!frameReady)
        throw std::runtime_error("Cannot get D3D12 swap-chain frame latency event");
    }
    return resize(w, h, error);
  } catch (const std::exception &e) {
    error = e.what();
    return false;
  }
}
bool Dx12Output::resize(unsigned w, unsigned h, std::string &error) {
  try {
    if (!w || !h || w > 16384 || h > 16384)
      throw std::runtime_error("Invalid D3D12 output dimensions");
    if (hmd_) {
      if (w == width && h == height) return true;
      throw std::runtime_error("HMD output requires its native display dimensions");
    }
    if (composes()) {
      width = w;
      height = h;
      return true;
    }
    waitFor(completion_.Get(), submitted_, event_);
    readback_.reset();
    for (auto &buffer : buffers_)
      buffer.Reset();
    if (chain_) {
      DXGI_SWAP_CHAIN_DESC1 actual{};
      checked(chain_->GetDesc1(&actual), "Inspect D3D12 swap chain");
      if (actual.Width != w || actual.Height != h)
        checked(chain_->ResizeBuffers(0, w, h, DXGI_FORMAT_UNKNOWN,
                                      DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT),
                "Resize D3D12 swap chain");
      for (UINT i = 0; i < 2; ++i)
        checked(chain_->GetBuffer(i, IID_PPV_ARGS(&buffers_[i])), "Acquire D3D12 swap buffer");
    } else {
      auto memory = heap(D3D12_HEAP_TYPE_DEFAULT);
      D3D12_RESOURCE_DESC d{};
      d.Dimension = D3D12_RESOURCE_DIMENSION_TEXTURE2D;
      d.Width = w;
      d.Height = h;
      d.DepthOrArraySize = d.MipLevels = 1;
      d.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
      d.SampleDesc.Count = 1;
      checked(device_->CreateCommittedResource(&memory, D3D12_HEAP_FLAG_NONE, &d,
                                               D3D12_RESOURCE_STATE_COMMON, nullptr,
                                               IID_PPV_ARGS(&buffers_[0])),
              "Create D3D12 offscreen output");
    }
    width = w;
    height = h;
    return true;
  } catch (const std::exception &e) {
    width = height = 0;
    error = e.what();
    return false;
  }
}
ID3D12Resource *Dx12Output::target() {
  return buffers_[chain_ ? chain_->GetCurrentBackBufferIndex() : 0].Get();
}
void Dx12Output::begin() {
  waitFor(completion_.Get(), submitted_, event_);
  readback_.reset();
  checked(allocator_->Reset(), "Reset output allocator");
  checked(list_->Reset(allocator_.Get(), nullptr), "Reset output command list");
}
void Dx12Output::submit(ID3D12Fence *handoff, UINT64 value, std::unique_ptr<Readback> readback) {
  checked(list_->Close(), "Close output commands");
  const auto completionValue = ++submitted_;
  if (readback) {
    readback->value = completionValue;
    readback_ = std::move(readback);
  }
  ID3D12CommandList *commands[] = {list_.Get()};
  queue_->ExecuteCommandLists(1, commands);
  const auto returned = handoff ? queue_->Signal(handoff, value) : S_OK;
  checked(queue_->Signal(completion_.Get(), completionValue), "Signal output completion");
  checked(returned, "Return texture to wallpaper worker");
}
bool Dx12Output::copyFrame(ID3D12Resource *src, ID3D12Fence *fence, UINT64 done,
                           std::string &error) {
  if (hmd_ && !validating)
    return hmd_->copyFrame(src, fence, done, error);
  try {
    // A crashed worker must never leave an indefinite Wait on the host's queue.
    waitFor(fence, done, event_);
    if (!validating) {
      begin();
      inFlightFence_ = fence;
      inFlightSurface_ = src;
      auto *dest = target();
      transition(list_.Get(), src, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_COPY_SOURCE);
      transition(list_.Get(), dest, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_COPY_DEST);
      list_->CopyResource(dest, src);
      transition(list_.Get(), dest, D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_COMMON);
      transition(list_.Get(), src, D3D12_RESOURCE_STATE_COPY_SOURCE, D3D12_RESOURCE_STATE_COMMON);
      submit(fence, done + 1);
    } else {
      // Validation only waited on the CPU. No host queue references this frame.
      checked(fence->Signal(done + 1), "Return validated texture to wallpaper worker");
    }
    return true;
  } catch (const std::exception &e) {
    error = e.what();
    return false;
  }
}
bool Dx12Output::blankHmd(std::string &error) {
  return hmd_ && hmd_->blank(error);
}
bool Dx12Output::showCalibration(const std::vector<CalibrationMark> &marks, std::string &error) {
  return hmd_ && hmd_->showCalibration(marks, error);
}
HRESULT Dx12Output::present() {
  if (composes())
    return compositionPresent_;
  return chain_ ? chain_->Present(0, DXGI_PRESENT_DO_NOT_WAIT) : S_OK;
}
bool Dx12Output::saveFrame(const std::filesystem::path &path, std::string &error) {
  try {
    if (chain_ || composes() || hmd_)
      throw std::runtime_error("D3D12 readback requires offscreen output");
    auto desc = target()->GetDesc();
    D3D12_PLACED_SUBRESOURCE_FOOTPRINT footprint{};
    UINT64 total = 0;
    device_->GetCopyableFootprints(&desc, 0, 1, 0, &footprint, nullptr, nullptr, &total);
    auto memory = heap(D3D12_HEAP_TYPE_READBACK);
    D3D12_RESOURCE_DESC buffer{};
    buffer.Dimension = D3D12_RESOURCE_DIMENSION_BUFFER;
    buffer.Width = total;
    buffer.Height = buffer.DepthOrArraySize = buffer.MipLevels = 1;
    buffer.SampleDesc.Count = 1;
    buffer.Layout = D3D12_TEXTURE_LAYOUT_ROW_MAJOR;
    auto pending = std::make_unique<Readback>();
    checked(device_->CreateCommittedResource(&memory, D3D12_HEAP_FLAG_NONE, &buffer,
                                             D3D12_RESOURCE_STATE_COPY_DEST, nullptr,
                                              IID_PPV_ARGS(&pending->pixels)),
            "Create D3D12 readback");
    pending->device = device_;
    pending->queue = queue_;
    pending->allocator = allocator_;
    pending->commands = list_;
    pending->fence = completion_;
    pending->source = target();
    pending->work = CreateThreadpoolWork(Readback::retire, pending.get(), nullptr);
    if (!pending->work)
      throw std::runtime_error("Cannot create D3D12 readback cleanup work");
    auto *readback = pending->pixels.Get();
    begin();
    transition(list_.Get(), target(), D3D12_RESOURCE_STATE_COMMON,
               D3D12_RESOURCE_STATE_COPY_SOURCE);
    D3D12_TEXTURE_COPY_LOCATION dst{};
    dst.pResource = readback;
    dst.Type = D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT;
    dst.PlacedFootprint = footprint;
    D3D12_TEXTURE_COPY_LOCATION src{};
    src.pResource = target();
    src.Type = D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX;
    list_->CopyTextureRegion(&dst, 0, 0, 0, &src, nullptr);
    transition(list_.Get(), target(), D3D12_RESOURCE_STATE_COPY_SOURCE,
               D3D12_RESOURCE_STATE_COMMON);
    submit(nullptr, 0, std::move(pending));
    waitFor(completion_.Get(), submitted_, event_);
    void *pixels = nullptr;
    D3D12_RANGE range{0, SIZE_T(total)};
    checked(readback->Map(0, &range, &pixels), "Map D3D12 readback");
    bool ok = saveRgbaFrame(path, width, height, footprint.Footprint.RowPitch,
                            static_cast<BYTE *>(pixels), error);
    D3D12_RANGE none{};
    readback->Unmap(0, &none);
    readback_.reset();
    return ok;
  } catch (const std::exception &e) {
    error = e.what();
    return false;
  }
}
PlayerOutput::PlayerOutput() = default;
PlayerOutput::~PlayerOutput() = default;

void PlayerOutput::releaseResources() noexcept {
  dx12_.reset();
  hmd11_.reset();
  NativeRenderer::releaseResources();
}

bool PlayerOutput::deviceLost() const {
  return dx12_ ? dx12_->deviceLost() : NativeRenderer::deviceLost() ||
      (hmd11_ && FAILED(hmd11_->device()->GetDeviceRemovedReason()));
}

bool PlayerOutput::initialize(HWND window, unsigned w, unsigned h, bool offscreen,
                              std::string &error, bool hardware, GpuPreference preference,
                              bool use12, bool allowComposition, const std::wstring &hmdPath) {
  releaseResources();
  result_ = S_OK;
  if (!hmdPath.empty()) {
    if (use12) {
      auto output = std::make_unique<Dx12Output>();
      if (!output->initializeHmd(hmdPath, error)) return false;
      dx12_ = std::move(output);
      return true;
    }
    auto output = HmdOutput::open(hmdPath, error);
    if (!output) return false;
    const auto adapter = output->adapterLuid();
    if (!NativeRenderer::initialize(nullptr, HmdOutput::Width, HmdOutput::Height, true, error,
                                   true, preference, &adapter) ||
        !output->initializeD3D11Bridge(device(), context(), error)) {
      NativeRenderer::releaseResources();
      return false;
    }
    hmd11_ = std::move(output);
    return true;
  }
  if (!use12)
    return NativeRenderer::initialize(window, w, h, offscreen, error, hardware, preference);
  if (!offscreen && allowComposition) {
    auto composed = std::make_unique<Dx12Output>();
    std::string unavailable;
    if (composed->initializeComposition(window, w, h, preference, unavailable)) {
      dx12_ = std::move(composed);
      return true;
    }
  }
  auto next = std::make_unique<Dx12Output>();
  if (!next->initialize(offscreen ? nullptr : window, w, h, offscreen && !hardware, preference,
                        error))
    return false;
  dx12_ = std::move(next);
  return true;
}
bool PlayerOutput::blankHmd(std::string &error) {
  return hmd11_ ? hmd11_->blank(error) : dx12_ && dx12_->blankHmd(error);
}
bool PlayerOutput::showCalibration(const std::vector<CalibrationMark> &marks, std::string &error) {
  return hmd11_ ? hmd11_->showCalibration(marks, error) : dx12_ && dx12_->showCalibration(marks, error);
}
bool PlayerOutput::present(std::string &error) {
  if (hmd11_) {
    if (!hmd11_->presentD3D11(outputTexture(), error)) return false;
    ++completed_;
    return true;
  }
  if (!dx12_)
    return NativeRenderer::present(error);
  result_ = dx12_->present();
  if (presentPending()) {
    ++busy_;
    return true;
  }
  if (FAILED(result_)) {
    error = "D3D12 present failed (" + std::to_string(result_) + ")";
    return false;
  }
  ++completed_;
  return true;
}
bool PlayerOutput::validateFrame(const std::function<bool()> &draw, std::string &error) {
  if (!dx12_)
    return NativeRenderer::validateFrame(draw, error);
  dx12_->validating = true;
  struct Reset {
    bool &flag;
    ~Reset() { flag = false; }
  } reset{dx12_->validating};
  return draw();
}
} // namespace action75
