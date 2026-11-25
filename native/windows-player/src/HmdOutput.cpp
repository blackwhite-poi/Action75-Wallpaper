#include <windows.h>
#include <dxgi1_6.h>
#include <windows.devices.display.core.interop.h>
#include <winrt/Windows.Foundation.Collections.h>
#include <winrt/Windows.Graphics.DirectX.Direct3D11.h>

#include "HmdOutput.h"

#include <algorithm>
#include <chrono>
#include <cmath>
#include <iostream>
#include <limits>
#include <optional>
#include <stdexcept>
#include <string_view>

namespace action75 {
namespace {
using namespace winrt;
using namespace Windows::Devices::Display;
using namespace Windows::Devices::Display::Core;
using namespace Windows::Graphics::DirectX;

constexpr auto RenderFormat = DXGI_FORMAT_R8G8B8A8_UNORM;
constexpr auto DisplayFormat = DirectXPixelFormat::R8G8B8A8UIntNormalized;

bool isAction75Hmd(const DisplayTarget &target) {
  if (!target.IsConnected()) return false;
  const auto usage = target.UsageKind();
  const auto path = target.DeviceInterfacePath();
  return (usage == DisplayMonitorUsageKind::HeadMounted || usage == DisplayMonitorUsageKind::SpecialPurpose) &&
         std::wstring_view(path).find(L"DISPLAY#HIV1230#") != std::wstring_view::npos;
}

std::string describe(const hresult_error &error) {
  return to_string(error.message()) + " (HRESULT " + std::to_string(uint32_t(error.code())) + ")";
}

void require(bool condition, const wchar_t* error) {
  if (!condition) throw hresult_error(E_FAIL, error);
}

DisplayFence importFence(ID3D12Device* graphics, const DisplayDevice& display, ID3D12Fence* fence) {
  handle shared;
  check_hresult(graphics->CreateSharedHandle(fence, nullptr, GENERIC_ALL, nullptr, shared.put()));
  com_ptr<::IInspectable> imported;
  imported.capture(display.as<IDisplayDeviceInterop>(), &IDisplayDeviceInterop::OpenSharedHandle, shared.get());
  return imported.as<DisplayFence>();
}

void barrier(ID3D12GraphicsCommandList* list, ID3D12Resource* resource,
             D3D12_RESOURCE_STATES before, D3D12_RESOURCE_STATES after) {
  D3D12_RESOURCE_BARRIER transition{};
  transition.Type = D3D12_RESOURCE_BARRIER_TYPE_TRANSITION;
  transition.Transition = {resource, D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES, before, after};
  list->ResourceBarrier(1, &transition);
}

bool isAction75(std::wstring_view path) {
  constexpr std::wstring_view prefix = LR"(\\?\DISPLAY#HIV1230#)";
  return path.size() > prefix.size() &&
         _wcsnicmp(path.data(), prefix.data(), prefix.size()) == 0;
}

std::wstring deviceInstance(std::wstring path, std::wstring_view kind) {
  const auto suffix = path.rfind(L"#{");
  require(path.compare(0, 4, LR"(\\?\)") == 0 && suffix != std::wstring::npos &&
              suffix > 4 + kind.size() && path[4 + kind.size()] == L'#' &&
              _wcsnicmp(path.data() + 4, kind.data(), kind.size()) == 0,
          L"Invalid display device path");
  path = path.substr(4, suffix - 4);
  std::replace(path.begin(), path.end(), L'#', L'\\');
  require(path.find(L'"') == std::wstring::npos, L"Invalid display device instance");
  return path;
}

std::wstring adapterInstance(const DisplayTarget &target) {
  const auto luid = target.Adapter().Id();
  DISPLAYCONFIG_ADAPTER_NAME adapter{};
  adapter.header.type = DISPLAYCONFIG_DEVICE_INFO_GET_ADAPTER_NAME;
  adapter.header.size = sizeof(adapter);
  adapter.header.adapterId = {luid.LowPart, luid.HighPart};
  check_hresult(HRESULT_FROM_WIN32(DisplayConfigGetDeviceInfo(&adapter.header)));
  return deviceInstance(adapter.adapterDevicePath, L"PCI");
}

struct RegistryKey {
  HKEY value = nullptr;
  ~RegistryKey() { if (value) RegCloseKey(value); }
};

class Action75Classification {
public:
  Action75Classification(const std::wstring &instance, const DisplayTarget &target) {
    const auto params = L"SYSTEM\\CurrentControlSet\\Enum\\" + instance + L"\\Device Parameters";
    const auto overridePath = params + L"\\EDID_OVERRIDE";
    RegistryKey overrideKey;
    const auto overrideStatus =
        RegOpenKeyExW(HKEY_LOCAL_MACHINE, overridePath.c_str(), 0, KEY_READ, &overrideKey.value);
    require(overrideStatus == ERROR_FILE_NOT_FOUND || overrideStatus == ERROR_PATH_NOT_FOUND,
            L"Remove the Action75 EDID override before switching output modes");

    const auto monitor = target.TryGetMonitor();
    require(bool(monitor), L"Cannot read Action75 monitor identity");
    const auto edid = monitor.GetDescriptor(DisplayMonitorDescriptorKind::Edid);
    const auto serial = displaySerialFromEdid({edid.begin(), edid.end()});
    const auto tag = serial.find(L":text:");
    require(tag != std::wstring::npos, L"Action75 has no usable EDID serial number");
    const auto text = serial.substr(tag + 6);
    require(!text.empty() && std::all_of(text.begin(), text.end(), [](wchar_t c) {
      return (c >= L'0' && c <= L'9') || (c >= L'A' && c <= L'Z') || (c >= L'a' && c <= L'z');
    }), L"Action75 EDID serial is not supported for mode switching");

    const auto prefix = L"HIV1230" + text + L"_";
    RegistryKey store;
    check_hresult(HRESULT_FROM_WIN32(RegOpenKeyExW(HKEY_LOCAL_MACHINE,
        L"SYSTEM\\CurrentControlSet\\Control\\GraphicsDrivers\\MonitorDataStore",
        0, KEY_ENUMERATE_SUB_KEYS, &store.value)));
    std::wstring match;
    for (DWORD index = 0;; ++index) {
      wchar_t name[256]{};
      DWORD length = ARRAYSIZE(name);
      const auto status = RegEnumKeyExW(store.value, index, name, &length, nullptr, nullptr, nullptr, nullptr);
      if (status == ERROR_NO_MORE_ITEMS) break;
      check_hresult(HRESULT_FROM_WIN32(status));
      if (length <= prefix.size() || _wcsnicmp(name, prefix.c_str(), prefix.size()) != 0) continue;
      require(match.empty(), L"More than one Action75 classification cache matches this monitor");
      match.assign(name, length);
    }
    require(!match.empty(), L"Cannot find the Action75 classification cache");
    check_hresult(HRESULT_FROM_WIN32(RegOpenKeyExW(store.value, match.c_str(), 0,
        KEY_QUERY_VALUE | KEY_SET_VALUE, &key_.value)));
    DWORD old = 0, size = sizeof(old);
    const auto status = RegGetValueW(key_.value, nullptr, Flag, RRF_RT_REG_DWORD, nullptr, &old, &size);
    if (status != ERROR_FILE_NOT_FOUND) {
      check_hresult(HRESULT_FROM_WIN32(status));
      original_ = old;
    }
  }

  void set(std::optional<DWORD> value) const {
    const auto status = value
        ? RegSetValueExW(key_.value, Flag, 0, REG_DWORD,
              reinterpret_cast<const BYTE *>(&*value), sizeof(*value))
        : RegDeleteValueW(key_.value, Flag);
    if (!value && status == ERROR_FILE_NOT_FOUND) return;
    check_hresult(HRESULT_FROM_WIN32(status));
  }
  void restore() const { set(original_); }

private:
  static constexpr auto Flag = L"HMDDevicePresent";
  RegistryKey key_;
  std::optional<DWORD> original_;
};

bool restartDisplayDevice(const std::wstring &instance) {
  wchar_t system[MAX_PATH]{};
  const auto length = GetSystemDirectoryW(system, ARRAYSIZE(system));
  require(length > 0 && length < ARRAYSIZE(system), L"Cannot locate Windows system directory");
  const auto executable = std::wstring(system) + L"\\pnputil.exe";
  auto command = L"\"" + executable + L"\" /restart-device \"" + instance + L"\"";
  STARTUPINFOW startup{sizeof(startup)};
  PROCESS_INFORMATION launched{};
  if (!CreateProcessW(executable.c_str(), command.data(), nullptr, nullptr, FALSE, CREATE_NO_WINDOW,
                     nullptr, nullptr, &startup, &launched))
    throw_last_error();
  handle process(launched.hProcess), thread(launched.hThread);
  require(WaitForSingleObject(process.get(), INFINITE) == WAIT_OBJECT_0,
          L"Cannot wait for display device restart");
  DWORD result = 0;
  if (!GetExitCodeProcess(process.get(), &result)) throw_last_error();
  return result == 0;
}

bool modeReady(const DisplayTarget &target, const std::wstring &path, bool hmd) {
  if (hmd) return target.UsageKind() == DisplayMonitorUsageKind::HeadMounted;
  if (target.UsageKind() != DisplayMonitorUsageKind::Standard) return false;
  const auto active = monitors();
  return std::any_of(active.begin(), active.end(), [&](const Monitor &m) {
    return !m.primary && _wcsicmp(m.identity.path.c_str(), path.c_str()) == 0;
  });
}

bool waitForDisplayMode(const std::wstring &path, bool hmd, std::chrono::seconds timeout) {
  const auto deadline = std::chrono::steady_clock::now() + timeout;
  do {
    try {
      const auto manager = DisplayManager::Create(DisplayManagerOptions::None);
      for (const auto &target : manager.GetCurrentTargets()) {
        if (target.IsConnected() && _wcsicmp(target.DeviceInterfacePath().c_str(), path.c_str()) == 0 &&
            modeReady(target, path, hmd))
          return true;
      }
    } catch (const hresult_error &) {
      // Display.Core may be temporarily unavailable during a device restart.
    }
    Sleep(300);
  } while (std::chrono::steady_clock::now() < deadline);
  return false;
}
} // namespace

bool switchAction75DisplayMode(bool hmd) {
  using namespace std::chrono_literals;
  try {
    handle operation(CreateMutexW(nullptr, TRUE, L"Local\\Action75DisplayModeSwitch"));
    const auto mutexStatus = GetLastError();
    if (!operation) check_hresult(HRESULT_FROM_WIN32(mutexStatus));
    require(mutexStatus != ERROR_ALREADY_EXISTS, L"Another Action75 mode switch is already running");
    std::wstring path, instance, adapter;
    std::unique_ptr<Action75Classification> classification;
    {
      const auto manager = DisplayManager::Create(DisplayManagerOptions::None);
      DisplayTarget selected{nullptr};
      for (const auto &target : manager.GetCurrentTargets()) {
        if (!target.IsConnected() || !isAction75(target.DeviceInterfacePath())) continue;
        require(!selected, L"More than one Action75 display is connected");
        selected = target;
      }
      require(bool(selected), L"Cannot find a connected Action75 display");
      path = selected.DeviceInterfacePath().c_str();
      if (modeReady(selected, path, hmd)) return false;
      instance = deviceInstance(path, L"DISPLAY");
      adapter = adapterInstance(selected);
      classification = std::make_unique<Action75Classification>(instance, selected);
    }
    classification->set(hmd ? std::optional<DWORD>{1} : std::nullopt);
    bool adapterRestarted = false;
    try {
      restartDisplayDevice(instance);
      if (waitForDisplayMode(path, hmd, 8s)) return false;
      adapterRestarted = true;
      require(restartDisplayDevice(adapter), L"Windows could not restart the Action75 graphics adapter");
      require(waitForDisplayMode(path, hmd, 25s), L"Action75 did not enter the requested display mode");
      return true;
    } catch (...) {
      classification->restore();
      if (adapterRestarted)
        restartDisplayDevice(adapter);
      else
        restartDisplayDevice(instance);
      throw;
    }
  } catch (const hresult_error &error) {
    throw std::runtime_error(describe(error));
  }
}

void appendHmdMonitors(std::vector<Monitor> &monitors) {
  try {
    const auto manager = DisplayManager::Create(DisplayManagerOptions::None);
    for (const auto &target : manager.GetCurrentTargets()) {
      if (!isAction75Hmd(target)) continue;
      const auto path = target.DeviceInterfacePath();
      const auto display = target.TryGetMonitor();
      std::wstring serial;
      if (display) {
        const auto edid = display.GetDescriptor(DisplayMonitorDescriptorKind::Edid);
        serial = displaySerialFromEdid(std::vector<BYTE>(edid.begin(), edid.end()));
      }
      const RECT bounds{0, 0, HmdOutput::Width, HmdOutput::Height};
      monitors.push_back({L"HMD:" + std::wstring(path), bounds, bounds, false,
                          {std::wstring(path), std::move(serial)}, true});
    }
  } catch (const hresult_error &) {
    // Display.Core can be unavailable in older or remote sessions.
  }
}

std::unique_ptr<HmdOutput> HmdOutput::open(const std::wstring &path, std::string &error) {
  try {
    const auto manager = DisplayManager::Create(DisplayManagerOptions::None);
    for (const auto &target : manager.GetCurrentTargets()) {
      if (isAction75Hmd(target) && _wcsicmp(target.DeviceInterfacePath().c_str(), path.c_str()) == 0)
        return std::make_unique<HmdOutput>(manager, target, false);
    }
    error = "Action75 HMD is no longer connected";
  } catch (const hresult_error &failure) {
    error = describe(failure);
  }
  return {};
}

HmdOutput::HmdOutput(const DisplayManager& manager, const DisplayTarget& target, bool offscreen)
    : offscreen_(offscreen) {
  const auto adapterId = target.Adapter().Id();
  const LUID luid{adapterId.LowPart, adapterId.HighPart};
  com_ptr<IDXGIFactory6> factory;
  factory.capture(CreateDXGIFactory2, 0);
  com_ptr<IDXGIAdapter4> adapter;
  adapter.capture(factory, &IDXGIFactory6::EnumAdapterByLuid, luid);
  check_hresult(D3D12CreateDevice(adapter.get(), D3D_FEATURE_LEVEL_11_0, IID_PPV_ARGS(device_.put())));

  D3D12_COMMAND_QUEUE_DESC queueDesc{};
  queueDesc.Type = D3D12_COMMAND_LIST_TYPE_DIRECT;
  check_hresult(device_->CreateCommandQueue(&queueDesc, IID_PPV_ARGS(queue_.put())));
  check_hresult(device_->CreateFence(0, D3D12_FENCE_FLAG_NONE, IID_PPV_ARGS(copied_.put())));
  event_.attach(CreateEventW(nullptr, FALSE, FALSE, nullptr));
  check_bool(bool(event_));
  for (auto& buffer : buffers_) {
    check_hresult(device_->CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT,
                                                  IID_PPV_ARGS(buffer.commands.put())));
  }
  check_hresult(device_->CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, buffers_[0].commands.get(),
                                           nullptr, IID_PPV_ARGS(list_.put())));
  check_hresult(list_->Close());
  if (!offscreen_) {
    manager_ = manager;
    try {
      initializeScanout(manager, target);
    } catch (...) {
      if (target_) {
        try { manager_.ReleaseTarget(target_); } catch (...) {}
      }
      throw;
    }
    return;
  }

  const D3D12_HEAP_PROPERTIES heap{D3D12_HEAP_TYPE_DEFAULT, D3D12_CPU_PAGE_PROPERTY_UNKNOWN,
                                   D3D12_MEMORY_POOL_UNKNOWN, 1, 1};
  D3D12_RESOURCE_DESC texture{};
  texture.Dimension = D3D12_RESOURCE_DIMENSION_TEXTURE2D;
  texture.Width = Width;
  texture.Height = Height;
  texture.DepthOrArraySize = texture.MipLevels = 1;
  texture.Format = RenderFormat;
  texture.SampleDesc.Count = 1;
  for (auto& buffer : buffers_) {
    check_hresult(device_->CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &texture,
        D3D12_RESOURCE_STATE_COMMON, nullptr, IID_PPV_ARGS(buffer.texture.put())));
  }
}

HmdOutput::~HmdOutput() {
  if (target_) {
    try { manager_.ReleaseTarget(target_); } catch (...) {}
  }
}

void HmdOutput::initializeScanout(const DisplayManager& manager, const DisplayTarget& target) {
  const auto usage = target.UsageKind();
  require(usage == DisplayMonitorUsageKind::HeadMounted || usage == DisplayMonitorUsageKind::SpecialPurpose,
          L"Action75 is not classified as an HMD or specialized display");
  const auto acquired = manager.TryAcquireTargetsAndCreateEmptyState(single_threaded_vector<DisplayTarget>({target}));
  if (acquired.ErrorCode() != DisplayManagerResult::Success) {
    check_hresult(acquired.ExtendedErrorCode());
    require(false, L"Acquire Action75 display target");
  }
  target_ = target;
  const auto state = acquired.State();
  const auto path = state.ConnectTarget(target);
  path.SourcePixelFormat(DisplayFormat);
  path.IsInterlaced(false);
  path.Scaling(DisplayPathScaling::Identity);
  DisplayModeInfo selected{nullptr};
  double closest = std::numeric_limits<double>::infinity();
  for (const auto& candidate : path.FindModes(DisplayModeQueryOptions::None)) {
    const auto source = candidate.SourceResolution(), sink = candidate.TargetResolution();
    const auto rate = candidate.PresentationRate().VerticalSyncRate;
    if (source.Width != Width || source.Height != Height || sink.Width != Width || sink.Height != Height ||
        !rate.Denominator) continue;
    const double delta = std::abs(static_cast<double>(rate.Numerator) / rate.Denominator - 60.0);
    if (delta < closest) {
      selected = candidate;
      closest = delta;
    }
  }
  require(bool(selected), L"No native RGBA8 Action75 mode");
  path.ApplyPropertiesFromMode(selected);
  const auto result = state.TryApply(DisplayStateApplyOptions::FailIfStateChanged);
  if (result.Status() != DisplayStateOperationStatus::Success) {
    check_hresult(result.ExtendedErrorCode());
    require(false, L"Apply Action75 display mode");
  }
  scanoutDevice_ = manager.CreateDisplayDevice(target.Adapter());
  scanoutSource_ = scanoutDevice_.CreateScanoutSource(target);
  const auto interop = scanoutDevice_.as<IDisplayDeviceInterop>();
  tasks_ = scanoutDevice_.CreateTaskPool();
  check_hresult(device_->CreateFence(0, D3D12_FENCE_FLAG_SHARED, IID_PPV_ARGS(displayed_.put())));
  flipTo_ = importFence(device_.get(), scanoutDevice_, displayed_.get());
  const DisplayPrimaryDescription primaryDesc{Width, Height, DisplayFormat,
                                               DirectXColorSpace::RgbFullG22NoneP709, false, {1, 0}};
  for (auto& buffer : buffers_) {
    const auto primary = scanoutDevice_.CreatePrimary(target, primaryDesc);
    handle shared;
    check_hresult(interop->CreateSharedHandle(primary.as<::IInspectable>().get(), nullptr, GENERIC_ALL,
                                               nullptr, shared.put()));
    check_hresult(device_->OpenSharedHandle(shared.get(), IID_PPV_ARGS(buffer.texture.put())));
    buffer.scanout = scanoutDevice_.CreateSimpleScanout(scanoutSource_, primary, 0, 1);
    check_hresult(device_->CreateFence(0, D3D12_FENCE_FLAG_SHARED, IID_PPV_ARGS(buffer.available.put())));
    buffer.flipAway = importFence(device_.get(), scanoutDevice_, buffer.available.get());
  }
  const auto rate = selected.PresentationRate().VerticalSyncRate;
  std::cout << "HMD primary textures imported into D3D12, RGBA8 1920x720 @ "
            << static_cast<double>(rate.Numerator) / rate.Denominator << " Hz\n" << std::flush;
}

bool HmdOutput::initializeD3D11Bridge(ID3D11Device *device, ID3D11DeviceContext *context, std::string &error) {
  try {
    com_ptr<ID3D11Device> base;
    base.copy_from(device);
    const auto opener = base.as<ID3D11Device5>();
    com_ptr<ID3D11DeviceContext> immediate;
    immediate.copy_from(context);
    bridgeContext_ = immediate.as<ID3D11DeviceContext4>();
    const D3D12_HEAP_PROPERTIES heap{D3D12_HEAP_TYPE_DEFAULT, D3D12_CPU_PAGE_PROPERTY_UNKNOWN,
                                     D3D12_MEMORY_POOL_UNKNOWN, 1, 1};
    D3D12_RESOURCE_DESC desc{};
    desc.Dimension = D3D12_RESOURCE_DIMENSION_TEXTURE2D;
    desc.Width = Width;
    desc.Height = Height;
    desc.DepthOrArraySize = desc.MipLevels = 1;
    desc.Format = RenderFormat;
    desc.SampleDesc.Count = 1;
    desc.Flags = D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET | D3D12_RESOURCE_FLAG_ALLOW_SIMULTANEOUS_ACCESS;
    check_hresult(device_->CreateCommittedResource(&heap, D3D12_HEAP_FLAG_SHARED, &desc,
        D3D12_RESOURCE_STATE_COMMON, nullptr, IID_PPV_ARGS(bridge12_.put())));
    handle shared;
    check_hresult(device_->CreateSharedHandle(bridge12_.get(), nullptr, GENERIC_ALL, nullptr, shared.put()));
    check_hresult(opener->OpenSharedResource1(shared.get(), IID_PPV_ARGS(bridge11_.put())));
    check_hresult(device_->CreateFence(0, D3D12_FENCE_FLAG_SHARED, IID_PPV_ARGS(bridgeFence12_.put())));
    handle fence;
    check_hresult(device_->CreateSharedHandle(bridgeFence12_.get(), nullptr, GENERIC_ALL, nullptr, fence.put()));
    check_hresult(opener->OpenSharedFence(fence.get(), IID_PPV_ARGS(bridgeFence11_.put())));
    return true;
  } catch (const hresult_error &failure) {
    error = describe(failure);
    return false;
  }
}

bool HmdOutput::presentD3D11(ID3D11Texture2D *texture, std::string &error) {
  try {
    bridgeContext_->CopyResource(bridge11_.get(), texture);
    const auto ready = ++bridgeFrame_ * 2 - 1;
    check_hresult(bridgeContext_->Signal(bridgeFence11_.get(), ready));
    bridgeContext_->Flush();
    return copyFrame(bridge12_.get(), bridgeFence12_.get(), ready, error);
  } catch (const hresult_error &failure) {
    error = describe(failure);
    return false;
  }
}

bool HmdOutput::wait(ID3D12Fence* fence, UINT64 value) {
  const auto completed = fence->GetCompletedValue();
  if (completed == UINT64_MAX) return false;
  if (completed >= value) return true;
  ResetEvent(event_.get());
  return SUCCEEDED(fence->SetEventOnCompletion(value, event_.get())) &&
         WaitForSingleObject(event_.get(), 1000) == WAIT_OBJECT_0 &&
         fence->GetCompletedValue() != UINT64_MAX && fence->GetCompletedValue() >= value;
}

bool HmdOutput::startVblankFence() {
  try {
    // Display.Core requires an active target. Create the clock after the first present
    // has been accepted, and keep the DisplayFence alive alongside its D3D12 handle.
    periodicFence_ = scanoutDevice_.CreatePeriodicFence(target_, {});
    const auto interop = scanoutDevice_.as<IDisplayDeviceInterop>();
    handle shared;
    check_hresult(interop->CreateSharedHandle(periodicFence_.as<::IInspectable>().get(), nullptr,
                                               GENERIC_ALL, nullptr, shared.put()));
    check_hresult(device_->OpenSharedHandle(shared.get(), IID_PPV_ARGS(vblank_.put())));
    return true;
  } catch (const hresult_error& error) {
    std::cerr << "VBlank fence unavailable: " << describe(error) << '\n';
    return false;
  }
}

bool HmdOutput::waitForVblank() {
  if (!vblank_) return false;
  const auto value = vblank_->GetCompletedValue();
  if (value != UINT64_MAX && wait(vblank_.get(), value + 1)) return true;
  std::cerr << "HMD source " << scanoutSource_.SourceId()
            << " VBlank fence did not advance: " << value << " -> "
            << vblank_->GetCompletedValue() << '\n';
  return false;
}

bool HmdOutput::blank(std::string &error) {
  return presentPattern(nullptr, error);
}

bool HmdOutput::showCalibration(const std::vector<CalibrationMark> &marks, std::string &error) {
  return presentPattern(&marks, error);
}

bool HmdOutput::presentPattern(const std::vector<CalibrationMark> *marks, std::string &error) {
  try {
    const D3D12_HEAP_PROPERTIES heap{D3D12_HEAP_TYPE_DEFAULT, D3D12_CPU_PAGE_PROPERTY_UNKNOWN,
                                     D3D12_MEMORY_POOL_UNKNOWN, 1, 1};
    auto desc = buffers_.front().texture->GetDesc();
    desc.Flags = D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET;
    com_ptr<ID3D12Resource> texture;
    com_ptr<ID3D12Fence> ready;
    com_ptr<ID3D12DescriptorHeap> descriptors;
    com_ptr<ID3D12CommandAllocator> allocator;
    com_ptr<ID3D12GraphicsCommandList> clear;
    check_hresult(device_->CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &desc,
        D3D12_RESOURCE_STATE_RENDER_TARGET, nullptr, IID_PPV_ARGS(texture.put())));
    const D3D12_DESCRIPTOR_HEAP_DESC descriptorDesc{D3D12_DESCRIPTOR_HEAP_TYPE_RTV, 1};
    check_hresult(device_->CreateDescriptorHeap(&descriptorDesc, IID_PPV_ARGS(descriptors.put())));
    const auto rtv = descriptors->GetCPUDescriptorHandleForHeapStart();
    device_->CreateRenderTargetView(texture.get(), nullptr, rtv);
    check_hresult(device_->CreateFence(0, D3D12_FENCE_FLAG_NONE, IID_PPV_ARGS(ready.put())));
    check_hresult(device_->CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT,
                                                   IID_PPV_ARGS(allocator.put())));
    check_hresult(device_->CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, allocator.get(),
                                             nullptr, IID_PPV_ARGS(clear.put())));
    constexpr float black[]{0, 0, 0, 1};
    constexpr float background[]{0.025f, 0.032f, 0.045f, 1};
    clear->ClearRenderTargetView(rtv, marks ? background : black, 0, nullptr);
    if (marks) {
      std::vector<D3D12_RECT> ordinary, selected;
      const auto rect = [](LONG x, LONG y, LONG halfWidth, LONG halfHeight) {
        return D3D12_RECT{std::max(0L, x - halfWidth), std::max(0L, y - halfHeight),
                         std::min(LONG(Width), x + halfWidth + 1),
                         std::min(LONG(Height), y + halfHeight + 1)};
      };
      for (const auto &mark : *marks) {
        auto &cross = mark.selected ? selected : ordinary;
        const auto x = static_cast<LONG>(std::lround(mark.x * (Width - 1)));
        const auto y = static_cast<LONG>(std::lround(mark.y * (Height - 1)));
        const LONG arm = mark.selected ? 18 : 10;
        cross.push_back(rect(x, y, arm, 1));
        cross.push_back(rect(x, y, 1, arm));
      }
      constexpr float normal[]{0.34f, 0.65f, 0.72f, 1};
      constexpr float active[]{1, 0.69f, 0.25f, 1};
      if (!ordinary.empty())
        clear->ClearRenderTargetView(rtv, normal, static_cast<UINT>(ordinary.size()), ordinary.data());
      if (!selected.empty())
        clear->ClearRenderTargetView(rtv, active, static_cast<UINT>(selected.size()), selected.data());
    }
    barrier(clear.get(), texture.get(), D3D12_RESOURCE_STATE_RENDER_TARGET, D3D12_RESOURCE_STATE_COMMON);
    check_hresult(clear->Close());
    ID3D12CommandList *commands[]{clear.get()};
    queue_->ExecuteCommandLists(1, commands);
    check_hresult(queue_->Signal(ready.get(), 1));
    return copyFrame(texture.get(), ready.get(), 1, error);
  } catch (const hresult_error &failure) {
    error = describe(failure);
    return false;
  }
}

bool HmdOutput::copyFrame(ID3D12Resource* source, ID3D12Fence* workerFence, UINT64 workerDone, std::string& error) {
  try {
    auto& buffer = buffers_[submitted_ % buffers_.size()];
    require(wait(workerFence, workerDone), L"Worker render fence timed out");
    require(wait(copied_.get(), buffer.lastCopy), L"D3D12 copy fence timed out");
    if (buffer.lastPresent && !vblankPaced_) {
      require(wait(buffer.available.get(), buffer.lastPresent), L"HMD flip-away fence timed out");
    }
    const auto frame = submitted_ + 1;
    source_.copy_from(source);
    workerFence_.copy_from(workerFence);
    check_hresult(buffer.commands->Reset());
    check_hresult(list_->Reset(buffer.commands.get(), nullptr));
    barrier(list_.get(), source, D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_COPY_SOURCE);
    barrier(list_.get(), buffer.texture.get(), D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_COPY_DEST);
    list_->CopyResource(buffer.texture.get(), source);
    barrier(list_.get(), buffer.texture.get(), D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_COMMON);
    barrier(list_.get(), source, D3D12_RESOURCE_STATE_COPY_SOURCE, D3D12_RESOURCE_STATE_COMMON);
    check_hresult(list_->Close());
    ID3D12CommandList* work[]{list_.get()};
    queue_->ExecuteCommandLists(1, work);
    check_hresult(queue_->Signal(workerFence, workerDone + 1));
    check_hresult(queue_->Signal(copied_.get(), frame));
    buffer.lastCopy = frame;
    if (!offscreen_) {
      require(wait(copied_.get(), frame), L"Copy did not complete before HMD submission");
      const auto task = tasks_.CreateTask();
      task.SetScanout(buffer.scanout);
      if (!vblankPaced_) {
        task.SetSignal(DisplayTaskSignalKind::OnPresentFlipAway, buffer.flipAway);
        task.SetSignal(DisplayTaskSignalKind::OnPresentFlipTo, flipTo_);
      }
      const auto delivered = tasks_.TryExecuteTask(task);
      if (delivered.PresentStatus() != DisplayPresentStatus::Success) {
        error = "Display status " + std::to_string(static_cast<int>(delivered.PresentStatus()));
        return false;
      }
      buffer.lastPresent = lastPresent_ = delivered.PresentId();
      if (!submitted_) {
        std::cout << "First DisplayTask accepted: id=" << lastPresent_ << " source_status="
                  << static_cast<int>(delivered.SourceStatus()) << std::endl;
      }
      // Submit at most once per measured refresh in the fallback. The ring
      // retains each primary for multiple refresh intervals before reuse.
      if (vblankPaced_) require(waitForVblank(), L"HMD vertical-blank fence timed out");
    }
    submitted_ = frame;
    if (!offscreen_ && frame == 1 && !waitForLatestScanout()) {
      error = "Action75 did not report scanout or vertical blank";
      return false;
    }
    return true;
  } catch (const hresult_error& failure) {
    error = to_string(failure.message()) + " (HRESULT " + std::to_string(uint32_t(failure.code())) + ")";
    return false;
  }
}

bool HmdOutput::waitForLatestScanout() {
  if (!wait(copied_.get(), submitted_)) {
    std::cerr << "Copy fence not reached: " << copied_->GetCompletedValue() << " expected " << submitted_ << '\n';
    return false;
  }
  if (offscreen_ || vblankPaced_ || (lastPresent_ && wait(displayed_.get(), lastPresent_))) return true;
  std::cerr << "D3D12 copy completed; flip-to fence not reached: " << displayed_->GetCompletedValue()
            << " expected " << lastPresent_ << '\n';
  if (submitted_ == 1 && startVblankFence() && waitForVblank()) {
    vblankPaced_ = true;
    std::cout << "Experimental vertical-blank pacing enabled; per-frame flip confirmation unavailable" << std::endl;
    return true;
  }
  return false;
}

std::uint64_t HmdOutput::sampleOffscreenRgb(unsigned x, unsigned y) {
  constexpr unsigned span = 64, pitch = span * 4;
  require(offscreen_ && submitted_ != 0 && x >= span/2 && y >= span/2 &&
          x + span/2 <= Width && y + span/2 <= Height, L"Invalid offscreen sample coordinates");
  auto& buffer = buffers_[(submitted_ - 1) % buffers_.size()];
  require(wait(copied_.get(), buffer.lastCopy), L"Offscreen frame has not finished");
  const D3D12_HEAP_PROPERTIES heap{D3D12_HEAP_TYPE_READBACK, D3D12_CPU_PAGE_PROPERTY_UNKNOWN,
                                   D3D12_MEMORY_POOL_UNKNOWN, 1, 1};
  D3D12_RESOURCE_DESC description{};
  description.Dimension = D3D12_RESOURCE_DIMENSION_BUFFER;
  description.Width = span * pitch;
  description.Height = description.DepthOrArraySize = description.MipLevels = 1;
  description.SampleDesc.Count = 1;
  description.Layout = D3D12_TEXTURE_LAYOUT_ROW_MAJOR;
  com_ptr<ID3D12Resource> readback;
  check_hresult(device_->CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &description,
      D3D12_RESOURCE_STATE_COPY_DEST, nullptr, IID_PPV_ARGS(readback.put())));
  com_ptr<ID3D12Fence> finished;
  check_hresult(device_->CreateFence(0, D3D12_FENCE_FLAG_NONE, IID_PPV_ARGS(finished.put())));
  check_hresult(buffer.commands->Reset());
  check_hresult(list_->Reset(buffer.commands.get(), nullptr));
  D3D12_TEXTURE_COPY_LOCATION from{}, to{};
  from.pResource = buffer.texture.get();
  from.Type = D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX;
  to.pResource = readback.get();
  to.Type = D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT;
  to.PlacedFootprint.Footprint = {RenderFormat, span, span, 1, pitch};
  const D3D12_BOX region{x-span/2, y-span/2, 0, x+span/2, y+span/2, 1};
  barrier(list_.get(), buffer.texture.get(), D3D12_RESOURCE_STATE_COMMON, D3D12_RESOURCE_STATE_COPY_SOURCE);
  list_->CopyTextureRegion(&to, 0, 0, 0, &from, &region);
  barrier(list_.get(), buffer.texture.get(), D3D12_RESOURCE_STATE_COPY_SOURCE, D3D12_RESOURCE_STATE_COMMON);
  check_hresult(list_->Close());
  ID3D12CommandList* work[]{list_.get()};
  queue_->ExecuteCommandLists(1, work);
  check_hresult(queue_->Signal(finished.get(), 1));
  require(wait(finished.get(), 1), L"Wait for offscreen pixel sample");
  const D3D12_RANGE read{0, span * pitch}, wrote{0, 0};
  void* mapped = nullptr;
  check_hresult(readback->Map(0, &read, &mapped));
  const auto* pixels = static_cast<const std::uint8_t*>(mapped);
  std::uint64_t rgb = 0;
  for (unsigned i = 0; i < span * pitch; i += 4) rgb += pixels[i] + pixels[i+1] + pixels[i+2];
  readback->Unmap(0, &wrote);
  return rgb;
}
} // namespace action75
