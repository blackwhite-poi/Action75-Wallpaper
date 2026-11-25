#include <windows.h>
#include <winrt/Windows.Foundation.Collections.h>

#include "FramePacer.h"
#include "HmdOutput.h"
#include "NativePluginSession.h"
#include "RawKeyboard.h"

#include <chrono>
#include <filesystem>
#include <iostream>
#include <optional>
#include <stdexcept>
#include <string_view>

namespace {
using namespace action75;
using namespace std::chrono_literals;
using Clock = FramePacer::Clock;
using winrt::Windows::Devices::Display::Core::DisplayManager;
using winrt::Windows::Devices::Display::Core::DisplayManagerOptions;
using winrt::Windows::Devices::Display::Core::DisplayTarget;
constexpr auto Width = HmdOutput::Width, Height = HmdOutput::Height;

DisplayTarget findTarget(const DisplayManager& manager) {
  DisplayTarget selected{nullptr};
  for (const auto& target : manager.GetCurrentTargets()) {
    if (!target.IsConnected()) continue;
    const auto monitor = target.TryGetMonitor();
    if (!monitor || std::wstring_view(monitor.DeviceId()).find(L"DISPLAY#HIV1230#") == std::wstring_view::npos) continue;
    if (selected) throw std::runtime_error("More than one Action75 target; refusing to select one");
    selected = target;
  }
  if (!selected) throw std::runtime_error("Action75 target HIV1230 is disconnected");
  return selected;
}

LRESULT CALLBACK inputMessage(HWND window, UINT message, WPARAM wparam, LPARAM lparam) {
  if (message == WM_NCCREATE) {
    auto* create = reinterpret_cast<CREATESTRUCTW*>(lparam);
    SetWindowLongPtrW(window, GWLP_USERDATA, reinterpret_cast<LONG_PTR>(create->lpCreateParams));
  }
  auto* keyboard = reinterpret_cast<RawKeyboard*>(GetWindowLongPtrW(window, GWLP_USERDATA));
  if (keyboard && (message == WM_INPUT || message == WM_INPUT_DEVICE_CHANGE)) {
    keyboard->process(message, wparam, lparam);
  }
  return DefWindowProcW(window, message, wparam, lparam);
}

struct InputWindow {
  explicit InputWindow(RawKeyboard& keyboard) {
    const auto instance = GetModuleHandleW(nullptr);
    WNDCLASSW type{};
    type.hInstance = instance;
    type.lpfnWndProc = inputMessage;
    type.lpszClassName = L"Action75HmdBlackHoleProbe";
    winrt::check_bool(RegisterClassW(&type) != 0);
    window = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE, type.lpszClassName, L"", WS_POPUP,
                             0, 0, 0, 0, nullptr, nullptr, instance, &keyboard);
    winrt::check_bool(window != nullptr);
    if (!keyboard.attach(window)) {
      DestroyWindow(window);
      throw std::runtime_error(keyboard.lastError());
    }
  }
  ~InputWindow() { DestroyWindow(window); }
  HWND window = nullptr;
};

void run(const std::filesystem::path& root, bool offscreen, bool brightKeys, const DisplayManager& manager) {
  HmdOutput output(manager, findTarget(manager), offscreen);
  NativePluginSession worker;
  std::string error;
  const auto pack = root / "wallpapers-native" / "black-hole";
  // Offscreen control hides the moving background so a synthetic event is the only animation source.
  const std::string settings = offscreen ? R"({"backgroundOpacity":0,"holeOpacity":0})" :
      brightKeys ? R"({"skyMode":3,"backgroundBrightness":0.08,"lensStrength":0,"starBrightness":2,"starSize":1.8})" : "{}";
  if (!worker.start12(output.device(), pack, pack / "wallpaper.dll", Width, Height, settings, error)) {
    throw std::runtime_error(error);
  }
  std::cout << "Black Hole ABI=" << worker.renderAbi() << " worker_pid=" << worker.workerPid()
            << " offscreen=" << offscreen << std::endl;

  const auto configuration = PlayerConfig::load(root);
  const auto started = Clock::now();
  unsigned physicalDown = 0, physicalUp = 0, syntheticDown = 0, verifiedInputFrames = 0;
  bool inputPending = false, dirty = true;
  const auto seconds = [&] { return std::chrono::duration<double>(Clock::now() - started).count(); };
  const auto send = [&](const KeyEvent& key) {
    const A75Event event{sizeof(A75Event), static_cast<uint32_t>(key.down ? A75_KEY_DOWN : A75_KEY_UP), Width, Height,
                         seconds(), key.x, key.y, key.code.c_str(), nullptr};
    if (!worker.event(event, error)) return false;
    dirty = true;
    inputPending |= key.down;
    return true;
  };
  RawKeyboard keyboard(configuration, [&](const KeyEvent& key) {
    if (!error.empty() || !send(key)) return;
    if (key.down) ++physicalDown;
    else ++physicalUp;
  });
  const InputWindow input(keyboard);
  std::cout << "Raw Input selected collections=" << keyboard.selectedDeviceCount() << std::endl;
  FramePacer pacer;
  int32_t delay = -1;
  const auto deadline = started + (offscreen ? 4s : 55s);
  bool observedFirstFlip = false;
  std::uint64_t beforeInput = 0;
  std::optional<std::uint64_t> afterInput;
  while (Clock::now() < deadline) {
    MSG message;
    for (unsigned messages = 0; messages < 64 && PeekMessageW(&message, nullptr, 0, 0, PM_REMOVE); ++messages) {
      if (message.message == WM_QUIT) return;
      DispatchMessageW(&message);
    }
    if (!error.empty()) throw std::runtime_error(error);
    if (!syntheticDown && Clock::now() - started >= (offscreen ? 700ms : 5s)) {
      if (!send({"KeyA", 0x04, true, .22f, .45f})) throw std::runtime_error(error);
      ++syntheticDown;
    }
    const auto now = Clock::now();
    const auto plan = pacer.plan(now, true, dirty, delay >= 0, std::chrono::milliseconds(std::max(0, delay)));
    if (!plan.due) {
      const auto remaining = deadline - now;
      const auto timeout = std::min(plan.timed ? plan.remaining : remaining, Clock::duration(50ms));
      MsgWaitForMultipleObjectsEx(0, nullptr, FramePacer::ceilingMilliseconds(timeout), QS_ALLINPUT, MWMO_INPUTAVAILABLE);
      continue;
    }
    const auto copy = [&](ID3D12Resource* texture, ID3D12Fence* fence, UINT64 value, std::string& failure) {
      return output.copyFrame(texture, fence, value, failure);
    };
    if (!worker.render12(seconds(), delay, copy, error)) throw std::runtime_error(error);
    if (!observedFirstFlip || inputPending) {
      if (!output.waitForLatestScanout()) throw std::runtime_error("A frame did not reach scanout or offscreen completion");
      if (!observedFirstFlip) {
        if (offscreen) beforeInput = output.sampleOffscreenRgb(unsigned(.22f * Width), unsigned(.45f * Height));
        std::cout << (offscreen ? "First GPU copy complete" : output.hasPerFrameScanoutSignal()
                      ? "First HMD flip-to fence signaled" : "First HMD frame accepted; vertical blank observed")
                  << std::endl;
        observedFirstFlip = true;
      }
      if (inputPending) {
        ++verifiedInputFrames;
        std::cout << "Frame after input submitted: frame=" << output.frames() << " physical_down=" << physicalDown
                  << " synthetic_down=" << syntheticDown << " worker_next_frame_ms=" << delay << std::endl;
      }
      inputPending = false;
    }
    if (offscreen && syntheticDown && !afterInput && Clock::now() - started >= 950ms) {
      afterInput = output.sampleOffscreenRgb(unsigned(.22f * Width), unsigned(.45f * Height));
    }
    pacer.rendered(now, 60, delay >= 0, dirty);
    dirty = false;
  }
  if (!observedFirstFlip || !output.waitForLatestScanout()) throw std::runtime_error("Final frame was not verified");
  std::cout << "Completed frames=" << output.frames() << " physical_down=" << physicalDown
            << " physical_up=" << physicalUp << " synthetic_down=" << syntheticDown
            << " verified_input_frames=" << verifiedInputFrames << " output_pixel_readbacks=0"
            << " per_frame_flip_signal=" << output.hasPerFrameScanoutSignal() << std::endl;
  if (offscreen) {
    std::cout << "Separate offscreen pixel check: 64x64 RGB sum before=" << beforeInput
              << " after=" << afterInput.value_or(0) << std::endl;
    if (output.frames() < 3 || !verifiedInputFrames || !afterInput || *afterInput <= beforeInput) {
      throw std::runtime_error("Offscreen black-hole scene did not produce a brighter key region after its test event");
    }
  }
}
} // namespace

int wmain(int argc, wchar_t** argv) {
  const bool offscreen = argc == 3 && std::wstring_view(argv[1]) == L"--offscreen";
  const bool brightKeys = argc == 3 && std::wstring_view(argv[1]) == L"--hmd-bright-keys";
  if (argc != 3 || (!offscreen && !brightKeys && std::wstring_view(argv[1]) != L"--hmd")) {
    std::wcerr << L"Usage: Action75HmdBlackHoleProbe (--offscreen | --hmd | --hmd-bright-keys) <repository-root>\n";
    return 2;
  }
  try {
    winrt::init_apartment();
    const auto manager = DisplayManager::Create(DisplayManagerOptions::None);
    run(std::filesystem::absolute(argv[2]), offscreen, brightKeys, manager);
    manager.Close();
  } catch (const winrt::hresult_error& failure) {
    std::wcerr << L"HRESULT 0x" << std::hex << uint32_t(failure.code()) << L": " << failure.message().c_str() << '\n';
    return 1;
  } catch (const std::exception& failure) {
    std::cerr << failure.what() << '\n';
    return 1;
  }
}
