#pragma once

#include <d3d12.h>
#include <d3d11_4.h>
#include <winrt/Windows.Devices.Display.Core.h>
#include "MonitorSelection.h"

#include <array>
#include <memory>
#include <string>

namespace action75 {
void appendHmdMonitors(std::vector<Monitor> &monitors);
bool switchAction75DisplayMode(bool hmd);

struct CalibrationMark {
  float x, y;
  bool selected;
};

class HmdOutput {
public:
  static constexpr unsigned Width = 1920, Height = 720;
  using DisplayManager = winrt::Windows::Devices::Display::Core::DisplayManager;
  using DisplayTarget = winrt::Windows::Devices::Display::Core::DisplayTarget;

  HmdOutput(const DisplayManager&, const DisplayTarget&, bool offscreen);
  ~HmdOutput();
  static std::unique_ptr<HmdOutput> open(const std::wstring &path, std::string &error);
  ID3D12Device* device() const { return device_.get(); }
  LUID adapterLuid() const { return device_->GetAdapterLuid(); }
  bool initializeD3D11Bridge(ID3D11Device *, ID3D11DeviceContext *, std::string &error);
  bool presentD3D11(ID3D11Texture2D *, std::string &error);
  bool blank(std::string &error);
  bool showCalibration(const std::vector<CalibrationMark> &marks, std::string &error);
  bool copyFrame(ID3D12Resource*, ID3D12Fence*, UINT64 workerDone, std::string& error);
  bool waitForLatestScanout();
  std::uint64_t sampleOffscreenRgb(unsigned x, unsigned y);
  UINT64 frames() const { return submitted_; }
  bool hasPerFrameScanoutSignal() const { return !offscreen_ && !vblankPaced_; }

private:
  bool presentPattern(const std::vector<CalibrationMark> *marks, std::string &error);
  struct Buffer {
    winrt::com_ptr<ID3D12Resource> texture;
    winrt::com_ptr<ID3D12CommandAllocator> commands;
    winrt::Windows::Devices::Display::Core::DisplayScanout scanout{nullptr};
    winrt::Windows::Devices::Display::Core::DisplayFence flipAway{nullptr};
    winrt::com_ptr<ID3D12Fence> available;
    UINT64 lastPresent = 0, lastCopy = 0;
  };

  bool wait(ID3D12Fence*, UINT64);
  bool startVblankFence();
  bool waitForVblank();
  void initializeScanout(const DisplayManager&, const DisplayTarget&);
  bool offscreen_;
  DisplayManager manager_{nullptr};
  DisplayTarget target_{nullptr};
  winrt::com_ptr<ID3D12Device> device_;
  winrt::com_ptr<ID3D12CommandQueue> queue_;
  winrt::com_ptr<ID3D12GraphicsCommandList> list_;
  winrt::com_ptr<ID3D12Fence> copied_, displayed_, vblank_;
  winrt::com_ptr<ID3D12Resource> source_;
  winrt::com_ptr<ID3D12Fence> workerFence_;
  winrt::com_ptr<ID3D12Resource> bridge12_;
  winrt::com_ptr<ID3D12Fence> bridgeFence12_;
  winrt::com_ptr<ID3D11Texture2D> bridge11_;
  winrt::com_ptr<ID3D11Fence> bridgeFence11_;
  winrt::com_ptr<ID3D11DeviceContext4> bridgeContext_;
  UINT64 bridgeFrame_ = 0;
  winrt::handle event_;
  winrt::Windows::Devices::Display::Core::DisplayTaskPool tasks_{nullptr};
  winrt::Windows::Devices::Display::Core::DisplayDevice scanoutDevice_{nullptr};
  winrt::Windows::Devices::Display::Core::DisplaySource scanoutSource_{nullptr};
  winrt::Windows::Devices::Display::Core::DisplayFence flipTo_{nullptr};
  winrt::Windows::Devices::Display::Core::DisplayFence periodicFence_{nullptr};
  std::array<Buffer, 3> buffers_;
  UINT64 submitted_ = 0, lastPresent_ = 0;
  bool vblankPaced_ = false;
};
} // namespace action75
