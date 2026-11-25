#pragma once
#include "NativeRenderer.h"
#include <d3d12.h>
#include <dcomp.h>
#include <dxgi1_6.h>

namespace action75 {
class HmdOutput;
struct CalibrationMark;
class Dx12CompositionSurface {
public:
  Dx12CompositionSurface() = default;
  ~Dx12CompositionSurface();
  Dx12CompositionSurface(const Dx12CompositionSurface &) = delete;
  Dx12CompositionSurface &operator=(const Dx12CompositionSurface &) = delete;
  HANDLE handle() const { return handle_; }

private:
  friend class Dx12Output;
  HANDLE handle_ = nullptr;
  Microsoft::WRL::ComPtr<IUnknown> content_;
};

// Only the player links this implementation. ABI 1 wallpaper DLLs still use NativeRenderer.
class Dx12Output {
public:
  Dx12Output();
  ~Dx12Output();
  bool initialize(HWND window, unsigned w, unsigned h, bool warp, GpuPreference preference,
                  std::string &error);
  bool initializeComposition(HWND window, unsigned w, unsigned h, GpuPreference preference,
                             std::string &error);
  bool initializeHmd(const std::wstring &path, std::string &error);
  bool retargetComposition(HWND window, std::string &error);
  bool composes() const { return composition_ != nullptr; }
  bool hasWindowPresentation() const { return chain_ || composes(); }
  bool blankHmd(std::string &error);
  bool showCalibration(const std::vector<CalibrationMark> &marks, std::string &error);
  bool usesHmd() const { return hmd_ != nullptr; }
  LUID compositionAdapter() const { return compositionAdapter_; }
  std::shared_ptr<Dx12CompositionSurface> makeCompositionSurface(std::string &error);
  bool showComposition(const std::shared_ptr<Dx12CompositionSurface> &surface, std::string &error);
  void recordCompositionPresent(HRESULT presented) { compositionPresent_ = presented; }
  void clearComposition() noexcept;
  bool resize(unsigned w, unsigned h, std::string &error);
  bool copyFrame(ID3D12Resource *surface, ID3D12Fence *handoff, UINT64 workerDone,
                 std::string &error);
  bool saveFrame(const std::filesystem::path &path, std::string &error);
  HRESULT present();
  ID3D12Device *device() const { return device_.Get(); }
  bool deviceLost() const { return device_ && FAILED(device_->GetDeviceRemovedReason()); }
  unsigned width = 0, height = 0;
  std::wstring adapterName;
  uint64_t adapterLuid = 0;
  HANDLE frameReady = nullptr, removedEvent = nullptr;
  bool validating = false;

private:
  template <class T> using Ptr = Microsoft::WRL::ComPtr<T>;
  struct Readback;
  Ptr<ID3D12Device> device_;
  Ptr<ID3D12CommandQueue> queue_;
  Ptr<ID3D12CommandAllocator> allocator_;
  Ptr<ID3D12GraphicsCommandList> list_;
  Ptr<ID3D12Fence> completion_, inFlightFence_;
  Ptr<ID3D12Resource> inFlightSurface_;
  Ptr<IDXGISwapChain3> chain_;
  Ptr<ID3D12Resource> buffers_[2];
  Ptr<IDCompositionDesktopDevice> composition_;
  Ptr<IDCompositionTarget> compositionTarget_;
  HWND compositionWindow_ = nullptr;
  Ptr<IDCompositionVisual2> visual_;
  std::shared_ptr<Dx12CompositionSurface> shownSurface_;
  LUID compositionAdapter_{};
  HRESULT compositionPresent_ = S_OK;
  std::unique_ptr<Readback> readback_;
  std::unique_ptr<HmdOutput> hmd_;
  UINT64 submitted_ = 0;
  HANDLE event_ = nullptr;
  void begin();
  void submit(ID3D12Fence *handoff = nullptr, UINT64 value = 0,
              std::unique_ptr<Readback> readback = {});
  ID3D12Resource *target();
};

class PlayerOutput : public NativeRenderer {
public:
  PlayerOutput();
  ~PlayerOutput();
  Dx12Output *dx12() const { return dx12_.get(); }
  bool blankHmd(std::string &error);
  bool showCalibration(const std::vector<CalibrationMark> &marks, std::string &error);
  bool usesHmd() const { return hmd11_ || (dx12_ && dx12_->usesHmd()); }
  bool initialize(HWND window, unsigned w, unsigned h, bool offscreen, std::string &error,
                  bool hardware = false, GpuPreference preference = GpuPreference::Discrete,
                  bool use12 = false, bool allowComposition = true, const std::wstring &hmdPath = {});
  void releaseResources() noexcept;
  bool resize(unsigned w, unsigned h, std::string &error) {
    return dx12_ ? dx12_->resize(w, h, error) : NativeRenderer::resize(w, h, error);
  }
  unsigned width() const { return dx12_ ? dx12_->width : NativeRenderer::width(); }
  unsigned height() const { return dx12_ ? dx12_->height : NativeRenderer::height(); }
  const std::wstring &adapterName() const {
    return dx12_ ? dx12_->adapterName : NativeRenderer::adapterName();
  }
  uint64_t adapterLuid() const {
    return dx12_ ? dx12_->adapterLuid : NativeRenderer::adapterLuid();
  }
  HANDLE frameReady() const { return dx12_ ? dx12_->frameReady : NativeRenderer::frameReady(); }
  HANDLE deviceRemovedEvent() const {
    return dx12_ ? dx12_->removedEvent : NativeRenderer::deviceRemovedEvent();
  }
  bool hasRenderTarget() const {
    return dx12_ ? dx12_->width != 0 : NativeRenderer::hasRenderTarget();
  }
  bool deviceLost() const;
  bool present(std::string &error);
  HRESULT lastPresentResult() const {
    return dx12_ ? result_ : NativeRenderer::lastPresentResult();
  }
  bool presentPending() const { return lastPresentResult() == DXGI_ERROR_WAS_STILL_DRAWING; }
  uint64_t presentsCompleted() const { return NativeRenderer::presentsCompleted() + completed_; }
  uint64_t presentsBusy() const { return NativeRenderer::presentsBusy() + busy_; }
  bool validateFrame(const std::function<bool()> &draw, std::string &error);
  bool saveFrame(const std::filesystem::path &file, std::string &error) {
    return dx12_ ? dx12_->saveFrame(file, error) : NativeRenderer::saveFrame(file, error);
  }
private:
  std::unique_ptr<Dx12Output> dx12_;
  std::unique_ptr<HmdOutput> hmd11_;
  HRESULT result_ = S_OK;
  uint64_t completed_ = 0, busy_ = 0;
};
} // namespace action75
