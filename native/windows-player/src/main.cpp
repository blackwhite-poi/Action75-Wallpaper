#include "FireworksScene.h"
#include "Diagnostics.h"
#include "DisplayGuard.h"
#include "DesktopWallpaperLayer.h"
#include "EffectPalette.h"
#include "EffekseerBackend.h"
#include "FieldEffect.h"
#include "FrameLatencyGuard.h"
#include "FramePacer.h"
#include "GlassShatter.h"
#include "IpcServer.h"
#include "KeyboardTaskbar.h"
#include "KeycapEffect.h"
#include "MonitorSelection.h"
#include "Dx12Output.h"
#include "Dx12PresentCounters.h"
#include "Action75Wallpaper.h"
#include "NativePluginSession.h"
#include "PackSettings.h"
#include "PerfStats.h"
#include "PlayerConfig.h"
#include "ProceduralBurst.h"
#include "RawKeyboard.h"
#include "RuntimeIdentity.h"
#include "SpriteBurst.h"
#include "TrayMenu.h"
#include "WallpaperCatalog.h"
#include "WaterSurface.h"
#include <algorithm>
#include <array>
#include <cctype>
#include <chrono>
#include <cmath>
#include <cstdint>
#include <cwchar>
#include <dbt.h>
#include <filesystem>
#include <fstream>
#include <future>
#include <memory>
#include <nlohmann/json.hpp>
#include <psapi.h>
#include <shellapi.h>
#include <sstream>
#include <string_view>
#include <thread>
#include <utility>
#include <vector>
#include <windows.h>
#include <windowsx.h>
#include <wincodec.h>
#include <cwctype>

namespace fs = std::filesystem;
using json = nlohmann::json;
using namespace action75;
constexpr UINT CommandMessage = WM_APP + 31, TrayMessage = WM_APP + 32;
constexpr UINT_PTR CloseTimer = 97, KeyboardZOrderTimer = 98;
constexpr UINT_PTR MonitorTopologyTimer = 99;
constexpr auto MonitorDisconnectGrace = std::chrono::milliseconds(1500);
constexpr int SkippedTrigger = -2;
constexpr int ExitHotkey = 0x0A75;
static const UINT QuitSessionMessage = RegisterWindowMessageW(L"Action75.QuitSession");
static const UINT ClaimShortcutMessage = RegisterWindowMessageW(L"Action75.ClaimExitShortcut");
struct WindowNotification {
  const wchar_t *marker;
  UINT message;
  bool delivered = false;
};
static BOOL CALLBACK notifyMarkedWindow(HWND hwnd, LPARAM parameter) {
  auto &notice = *reinterpret_cast<WindowNotification *>(parameter);
  if (GetPropW(hwnd, notice.marker) && PostMessageW(hwnd, notice.message, 0, 0))
    notice.delivered = true;
  return TRUE;
}
static bool notifyManager(const std::wstring &endpoint, UINT message) {
  const auto marker = endpoint + L"-ManagerWindow";
  WindowNotification notice{marker.c_str(), message};
  EnumWindows(notifyMarkedWindow, reinterpret_cast<LPARAM>(&notice));
  return notice.delivered;
}
static std::string utf8(const std::wstring &s) {
  if (s.empty())
    return {};
  int n = WideCharToMultiByte(CP_UTF8, 0, s.data(), static_cast<int>(s.size()), nullptr, 0, nullptr,
                              nullptr);
  std::string r(n, 0);
  WideCharToMultiByte(CP_UTF8, 0, s.data(), static_cast<int>(s.size()), r.data(), n, nullptr,
                      nullptr);
  return r;
}
static bool processElevated() noexcept {
  static const bool elevated = [] {
    HANDLE token = nullptr;
    if (!OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &token))
      return false;
    TOKEN_ELEVATION value{};
    DWORD length = 0;
    const bool result =
        GetTokenInformation(token, TokenElevation, &value, sizeof(value), &length) &&
        value.TokenIsElevated;
    CloseHandle(token);
    return result;
  }();
  return elevated;
}
static std::wstring wide(const std::string &s) {
  if (s.empty())
    return {};
  int n = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, s.data(), static_cast<int>(s.size()),
                              nullptr, 0);
  if (!n)
    throw std::runtime_error("Invalid UTF-8");
  std::wstring r(n, 0);
  MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, s.data(), static_cast<int>(s.size()), r.data(),
                      n);
  return r;
}
static void stdoutJson(const json &result) {
  std::string text = result.dump() + "\n";
  DWORD count = 0;
  HANDLE out = GetStdHandle(STD_OUTPUT_HANDLE);
  if (out && out != INVALID_HANDLE_VALUE)
    WriteFile(out, text.data(), static_cast<DWORD>(text.size()), &count, nullptr);
}
static json readJson(const fs::path &file) {
  std::ifstream in(file, std::ios::binary);
  if (!in)
    throw std::runtime_error("Cannot read " + utf8(file.wstring()));
  in.seekg(0, std::ios::end);
  if (in.tellg() > 1024 * 1024)
    throw std::runtime_error("JSON file exceeds size limit");
  in.seekg(0);
  return json::parse(in);
}
static GpuPreference requestedGpu(const fs::path &root) {
  try {
    if (readJson(root / L"data/app-settings.json").value("gpuPreference", std::string{}) ==
        "integrated")
      return GpuPreference::Integrated;
  } catch (const std::exception &) {
    // Older installations and unconfigured players use the default preference.
  }
  return GpuPreference::Discrete;
}
static std::array<std::uint8_t, 3> effectColor(const json &value) {
  if (!value.is_array() || value.size() != 3)
    throw std::runtime_error("Effect color must contain three RGB integers (0..255)");
  std::array<std::uint8_t, 3> color{};
  for (std::size_t i = 0; i < color.size(); ++i) {
    if (!value[i].is_number_integer())
      throw std::runtime_error("Effect color must contain RGB integers (0..255)");
    const auto channel = value[i].get<std::int64_t>();
    if (channel < 0 || channel > 255)
      throw std::runtime_error("Effect color channels must be between 0 and 255");
    color[i] = static_cast<std::uint8_t>(channel);
  }
  return color;
}
static std::vector<EffectRgb> effectPaletteColors(const json &value) {
  if (!value.is_array() || value.empty() || value.size() > 9)
    throw std::runtime_error("Custom palette must contain between 1 and 9 RGB colors");
  std::vector<EffectRgb> colors;
  for (const auto &color : value)
    colors.push_back(effectColor(color));
  return colors;
}
static float normalizedEffectHue(const std::vector<EffectRgb> &resolvedColors, float hue) {
  return resolvedColors.size() == 1 ? hue : 0;
}
static void writeJson(const fs::path &file, const json &document) {
  fs::create_directories(file.parent_path());
  auto temporary = file;
  temporary += L".tmp";
  {
    std::ofstream out(temporary, std::ios::binary | std::ios::trunc);
    std::string contents = document.dump(2);
    out.write(contents.data(), static_cast<std::streamsize>(contents.size()));
    out.close();
    if (!out)
      throw std::runtime_error("Cannot save settings");
  }
  if (!MoveFileExW(temporary.c_str(), file.c_str(),
                   MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH))
    throw std::runtime_error("Cannot atomically replace settings");
}
static fs::path defaultRoot() {
  wchar_t file[32768];
  DWORD count = GetModuleFileNameW(nullptr, file, 32768);
  fs::path dir = fs::path(std::wstring(file, count)).parent_path();
  for (int n = 0; n < 5; ++n, dir = dir.parent_path())
    if (fs::exists(dir / L"existing-resources/finalmouse/action75-ue-keymap.json"))
      return dir;
  throw std::runtime_error("Project root not found; use --root");
}
enum class TestWindow { None, Native, Shared };
struct Options {
  fs::path root, stateDir;
  std::wstring pipe, rootEndpoint;
  std::string command;
  bool selfTest = false, headless = false, probe = false, hardwareHeadless = false,
       perfStats = false, dx12PresentStats = false;
  bool preferVolumeAtlas = false, backdropCopy = true;
  bool importTrial = false, integratedTrial = false;
  bool explicitPipe = false;
  TestWindow testWindow = TestWindow::None;
};
static Options options() {
  Options o;
  int argc = 0;
  LPWSTR *argv = CommandLineToArgvW(GetCommandLineW(), &argc);
  for (int i = 1; i < argc; ++i) {
    std::wstring arg = argv[i];
    auto value = [&]() {
      if (++i >= argc)
        throw std::runtime_error("Missing argument");
      return std::wstring(argv[i]);
    };
    if (arg == L"--root")
      o.root = value();
    else if (arg == L"--state-dir")
      o.stateDir = value();
    else if (arg == L"--pipe") {
      o.pipe = value();
      o.explicitPipe = true;
    } else if (arg == L"--command")
      o.command = utf8(value());
    else if (arg == L"--self-test")
      o.selfTest = true;
    else if (arg == L"--headless-test")
      o.headless = true;
    else if (arg == L"--headless-hardware")
      o.hardwareHeadless = true;
    else if (arg == L"--headless-window") {
      const auto route = value();
      if (route == L"native")
        o.testWindow = TestWindow::Native;
      else if (route == L"shared")
        o.testWindow = TestWindow::Shared;
      else
        throw std::runtime_error("Unknown diagnostic DX12 output route");
    }
    else if (arg == L"--perf-stats")
      o.perfStats = true;
    else if (arg == L"--dx12-present-stats")
      o.dx12PresentStats = true;
    else if (arg == L"--probe")
      o.probe = true;
    else if (arg == L"--integrated-gpu")
      o.integratedTrial = true;
    else if (arg == L"--import-trial")
      o.importTrial = true;
    else if (arg == L"--volume-atlas")
      o.preferVolumeAtlas = true;
    else if (arg == L"--shader-backdrop")
      o.backdropCopy = false;
    else
      throw std::runtime_error("Unknown command-line option");
  }
  LocalFree(argv);
  if (o.testWindow != TestWindow::None) {
    wchar_t desktop[128]{};
    if (!o.headless || !o.hardwareHeadless || o.importTrial ||
        !GetUserObjectInformationW(GetThreadDesktop(GetCurrentThreadId()), UOI_NAME,
                                   desktop, sizeof(desktop), nullptr) ||
        std::wstring_view(desktop).find(L"Action75MemQA-") != 0)
      throw std::runtime_error("Window tests require an isolated Action75MemQA desktop");
  }
  if (o.integratedTrial && !o.importTrial)
    throw std::runtime_error("--integrated-gpu applies only to --import-trial");
  if (o.importTrial) {
    if (o.root.empty())
      throw std::runtime_error("Wallpaper import trial requires a staging directory");
    o.root = fs::absolute(o.root).lexically_normal();
    o.stateDir = o.root / L"trial-state";
    o.headless = true;
    return o;
  }
  if (o.root.empty())
    o.root = defaultRoot();
  o.root = fs::absolute(o.root).lexically_normal();
  o.rootEndpoint = installationEndpoint(o.root);
  if (!o.explicitPipe)
    o.pipe = o.rootEndpoint;
  if (o.stateDir.empty())
    o.stateDir = o.root / L"data";
  if (o.headless && (!o.explicitPipe || o.pipe.empty() ||
                     _wcsicmp(o.pipe.c_str(), L"Action75NativePlayer") == 0))
    throw std::runtime_error("Headless tests require an isolated --pipe name");
  if (o.headless && o.stateDir == o.root / L"data")
    throw std::runtime_error("Headless tests require an isolated --state-dir");
  return o;
}

static json commandForRuntime(const Options &options) {
  auto request = json::parse(options.command);
  if (!request.is_object())
    throw std::runtime_error("Expected JSON object");
  if (request.contains("expectedRuntimeRoot") &&
      !matchesRuntimeRoot(fs::u8path(request.at("expectedRuntimeRoot").get<std::string>()),
                          options.rootEndpoint))
    throw std::runtime_error("Player directory changed");
  const auto root = utf8(options.root.wstring());
  request["expectedRuntimeRoot"] = root;
  const bool queryingStatus = request.at("action") == "status";
  const auto verifiedPid = [&](const json &reply) {
    const auto &status = reply.at("result");
    if (!matchesRuntimeRoot(fs::u8path(status.at("runtimeRoot").get<std::string>()),
                            options.rootEndpoint))
      throw std::runtime_error("Player directory changed");
    const auto pid = status.at("playerPid").get<DWORD>();
    if (!pid)
      throw std::runtime_error("Invalid player identity");
    return pid;
  };

  DWORD peer = 0;
  if (options.explicitPipe && !queryingStatus) {
    const auto proof = sendCommand(options.pipe, {{"action", "status"}, {"expectedRuntimeRoot", root}});
    if (!proof.value("ok", false))
      return proof;
    peer = verifiedPid(proof);
  }
  auto reply = sendCommand(options.pipe, request, peer);
  if (queryingStatus && reply.value("ok", false))
    verifiedPid(reply);
  return reply;
}
// Insert the wallpaper directly above the keyboard's taskbar. Keep the rest of
// Explorer's z-order, including the primary taskbar and its popups.
static void keepKeyboardAboveTaskbar(HWND wallpaper) {
  if (!IsWindowVisible(wallpaper))
    return;
  const auto monitor = MonitorFromWindow(wallpaper, MONITOR_DEFAULTTONULL);
  MONITORINFO info{sizeof(info)};
  RECT output{};
  if (!monitor || !GetMonitorInfoW(monitor, &info) || (info.dwFlags & MONITORINFOF_PRIMARY) ||
      !GetWindowRect(wallpaper, &output) || !EqualRect(&output, &info.rcMonitor))
    return;

  auto candidate = GetWindow(wallpaper, GW_HWNDPREV);
  for (int index = 0; candidate && index < 96; ++index) {
    if (IsWindowVisible(candidate) &&
        MonitorFromWindow(candidate, MONITOR_DEFAULTTONULL) == monitor) {
      wchar_t name[64]{};
      if (GetClassNameW(candidate, name, static_cast<int>(std::size(name))) &&
          std::wcscmp(name, L"Shell_SecondaryTrayWnd") == 0) {
        RECT bar{}, overlap{};
        if (GetWindowRect(candidate, &bar) && IntersectRect(&overlap, &output, &bar)) {
          const auto aboveTaskbar = GetWindow(candidate, GW_HWNDPREV);
          SetWindowPos(wallpaper, aboveTaskbar ? aboveTaskbar : HWND_TOPMOST, 0, 0, 0, 0,
                       SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER);
          return;
        }
      }
    }
    candidate = GetWindow(candidate, GW_HWNDPREV);
  }
}
struct ImportedPlayback {
  std::string lastSuccessful, attempted;
};
static fs::path importedPlaybackFile(const fs::path &stateDir, const std::string &id) {
  if (!PackSettingsStore::validPackId(id))
    throw std::runtime_error("Invalid wallpaper ID");
  return stateDir / L"wallpaper-versions" / fs::u8path(id + ".json");
}
static ImportedPlayback readImportedPlayback(const fs::path &stateDir, const std::string &id) {
  const auto file = importedPlaybackFile(stateDir, id);
  if (!fs::exists(file))
    return {};
  const auto document = readJson(file);
  if (document.value("version", 0) != 1)
    throw std::runtime_error("Invalid imported wallpaper playback record");
  ImportedPlayback playback{document.at("lastSuccessfulRevision").get<std::string>(),
                            document.value("attemptedRevision", std::string())};
  if (!validImportedRevision(playback.lastSuccessful) ||
      (!playback.attempted.empty() && !validImportedRevision(playback.attempted)))
    throw std::runtime_error("Invalid imported wallpaper revision");
  return playback;
}
static void writeImportedPlayback(const fs::path &stateDir, const std::string &id,
                                  const ImportedPlayback &playback) {
  writeJson(importedPlaybackFile(stateDir, id),
            {{"version", 1}, {"lastSuccessfulRevision", playback.lastSuccessful},
             {"attemptedRevision", playback.attempted}});
}
using CatalogScan = std::shared_future<std::shared_ptr<WallpaperCatalogSnapshot>>;

struct CommandCall {
  const json *request;
  json response;
  CatalogScan *legacyCatalogWait = nullptr;
};

static double clockSeconds(std::chrono::steady_clock::time_point time) noexcept {
  return std::chrono::duration<double>(time.time_since_epoch()).count();
}

struct Scene {
  std::unique_ptr<SpriteBurst> sprite;
  std::unique_ptr<KeycapEffect> keycaps;
  std::unique_ptr<FieldEffect> field;
  std::unique_ptr<ProceduralBurst> procedural;
  std::unique_ptr<WaterSurface> water;
  std::unique_ptr<FireworksScene> fireworks;
  std::unique_ptr<GlassShatter> glass;
  std::shared_ptr<Dx12CompositionSurface> compositionSurface;
  std::unique_ptr<NativePluginSession> plugin;
  int32_t pluginDelay = -1;

  bool load(const Pack &pack, PlayerOutput &renderer, const Options &options,
            const json &settings, std::string &error) {
    auto *device = renderer.device();
    if (pack.kind == "native") {
      plugin = std::make_unique<NativePluginSession>();
      if (auto *output = renderer.dx12()) {
        if (output->composes()) {
          compositionSurface = output->makeCompositionSurface(error);
          return compositionSurface &&
                 plugin->startComposition(output->compositionAdapter(), compositionSurface->handle(),
                                          pack.resourceDirectory, pack.nativeModule, renderer.width(),
                                          renderer.height(), settings.dump(), error, !pack.desktopOpacityControl.empty());
        }
        return plugin->start12(output->device(),pack.resourceDirectory,pack.nativeModule,renderer.width(),
                               renderer.height(),settings.dump(),error);
      }
      return plugin->start(device, pack.resourceDirectory, pack.nativeModule, renderer.width(),
                           renderer.height(), settings.dump(), error);
    }
    if (pack.kind == "waterSurface") {
      water = std::make_unique<WaterSurface>();
      return water->load(device, renderer.context(), renderer.width(), renderer.height(), error);
    }
    if (pack.kind == "fireworks") {
      fireworks = std::make_unique<FireworksScene>();
      return fireworks->load(device, renderer.width(), renderer.height(), error);
    }
    if (pack.kind == "glassShatter") {
      glass = options.headless ? std::make_unique<GlassShatter>(0xa751u)
                               : std::make_unique<GlassShatter>();
      return glass->load(device, error);
    }
    if (pack.kind == "proceduralBurst") {
      procedural = std::make_unique<ProceduralBurst>();
      return procedural->load(device, pack.proceduralStyle, pack.sprite.diameter,
                              pack.sprite.duration, error, pack.filmShapes);
    }
    if (pack.kind == "field") {
      field = std::make_unique<FieldEffect>();
      return field->load(device, pack.layout, pack.fieldTheme, error);
    }
    if (pack.kind == "keycap") {
      keycaps = std::make_unique<KeycapEffect>();
      return keycaps->load(device, pack.layout, error);
    }
    if (pack.kind == "spriteBurst") {
      sprite = std::make_unique<SpriteBurst>();
      return sprite->load(device, pack.effect, pack.sprite, error, !options.preferVolumeAtlas);
    }
    return true;
  }

  bool empty() const noexcept {
    return !sprite && !keycaps && !field && !procedural && !water && !fireworks && !glass && !plugin;
  }

  void configure(const PackAppearance &appearance) const {
    if (water)
      water->configure(appearance);
    if (fireworks)
      fireworks->configure(appearance);
    if (glass)
      glass->configure(appearance);
    if (procedural)
      procedural->configure(appearance);
    if (field)
      field->configure(appearance);
    if (keycaps)
      keycaps->configure(appearance);
  }

  bool draw(PlayerOutput &renderer, double now, bool paused, std::string &error) {
    if (auto *output=renderer.dx12()) {
      if (!plugin) return false;
      if (compositionSurface) {
        HRESULT presented = S_OK;
        if (!output->showComposition(compositionSurface, error) ||
            !plugin->renderComposition(now, pluginDelay, output->validating, presented, error))
          return false;
        if (!output->validating)
          output->recordCompositionPresent(presented);
        return true;
      }
      return plugin->render12(now,pluginDelay,[output](auto *surface,auto *fence,UINT64 value,std::string &failure) {
        return output->copyFrame(surface,fence,value,failure);
      },error);
    }
    if (plugin)
      return plugin->render(renderer.outputTexture(), now, pluginDelay, error);
    renderer.beginFrame(water || fireworks || glass);
    auto *context = renderer.context();
    const auto width = renderer.width(), height = renderer.height();
    if (water)
      return water->draw(context, width, height, now, paused, error);
    if (fireworks)
      return fireworks->draw(context, width, height, now, error);
    if (glass)
      return glass->draw(context, width, height, now, error);
    if (procedural)
      return procedural->draw(context, width, height, now, paused, error);
    if (field)
      return field->draw(context, width, height, now, paused, error);
    if (keycaps)
      return keycaps->draw(context, width, height, now, error);
    if (sprite && !paused)
      return sprite->draw(context, width, height, now, error);
    return true;
  }
};

class Player {
public:
  explicit Player(Options options)
      : options_(std::move(options)),
        mapping_(options_.importTrial ? PlayerConfig{} : PlayerConfig::load(options_.root)),
        settings_(options_.stateDir) {
    if (options_.headless)
      randomState_ = 0x9e3779b97f4a7c15ull;
  }
  int run();
  json trialWallpaper();
  json dispatch(const json &request);
  LRESULT message(HWND window, UINT message, WPARAM w, LPARAM l);

private:
  json status() const;
  json listMonitors(const std::vector<Monitor> &displays) const;
  json listDevices(bool refresh = false);
  json listPacks(const json &request);
  json catalogEntry(const Pack &pack) const;
  json listPalettes() const;
  const std::vector<EffectRgb> &activePalette() const;
  void openManager(const wchar_t *argument = nullptr);
  void quitSession(HWND window);
  void claimExitShortcut(HWND window);
  void registerTrayIcon();
  void saveSettings();
  void chooseMonitor(bool persistBinding = false) {
    chooseMonitor(persistBinding, testMonitors_ ? *testMonitors_ : monitors());
  }
  void chooseMonitor(bool persistBinding, const std::vector<Monitor> &displays);
  bool offscreenOnly() const { return options_.headless && !testMonitors_; }
  bool usesOutputWindow() const { return !options_.headless || options_.testWindow != TestWindow::None; }
  bool gpuDeviceLost() const {
    return renderer_.deviceLost() || (scene_.plugin && scene_.plugin->compositionDeviceLost());
  }
  bool outputReady() const {
    return hasMonitor_ && !graphicsSuspended_ && !outputRetryAt_ && renderer_.hasRenderTarget();
  }
  int handleKey(const KeyEvent &event);
  bool resizeOutput(unsigned width, unsigned height, std::string &error);
  bool selectPack(const std::string &id, std::string &error);
  bool selectCandidate(const Pack &pack, std::string &error);
  bool restoreSelection(const std::string &id, std::string &error);
  json protectedImportRevisions(const std::string &id) const;
  bool activatePack(const Pack *found, std::string &error, bool restoringDevice = false);
  bool activateOnCurrentOutput(const Pack *found,std::string &error,bool restoringDevice);
  bool initializeOutput(bool use12, unsigned width, unsigned height, bool temporary,
                        std::string &error, bool allowComposition = true);
  const Pack *selectedPack() const;
  bool importUpdatePending(const Pack &pack) const;
  bool setDesktopMode(bool transparent,std::string &error);
  void refreshDesktopLayer();
  void updateVisibility();
  void refreshDisplayGuard();
  void refreshKeyboardTaskbar();
  void stopEffects();
  void applyAppearance();
  void refreshPacks();
  void publishCatalog(WallpaperCatalogSnapshot &snapshot);
  void startCatalogScan();
  const Pack &requirePack(const std::string &id) const;
  void applyProfile(const PackSettingsCandidate &profile, const PackAppearance &appearance);
  void updateProfile(PackSettingsCandidate &profile, const PackAppearance &appearance,
                     std::optional<bool> persist);
  bool sendPluginEvent(uint32_t type, float x = 0, float y = 0, const char *code = "", const char *values = "{}");
  bool pluginPointerEnabled() const;
  void resetPluginPointer(bool cancel = true);
  void pluginFailed();
  void fallBackAfterWallpaperFailure();
  void retryMissingWallpaper(FramePacer::Time now);
  bool canClickScene() const;
  std::array<float, 2> pointerPosition(POINT client) const;
  bool validateUniforms(const std::vector<int> &effects, const PackAppearance &appearance,
                        std::string &error) const;
  int trigger(float x, float y, const std::string &code = {});
  void releaseGraphics();
  void suspendGraphics();
  void scheduleDeviceRecovery(const std::string &error);
  void recoverGraphics();
  bool restoreDevice(std::string &error);
  void render();
  bool hasMotion() const;
  unsigned renderFps() const { return 60; }
  static LRESULT CALLBACK procedure(HWND h, UINT m, WPARAM w, LPARAM l) {
    auto *self = reinterpret_cast<Player *>(GetWindowLongPtrW(h, GWLP_USERDATA));
    if (m == WM_NCCREATE) {
      self = static_cast<Player *>(reinterpret_cast<CREATESTRUCTW *>(l)->lpCreateParams);
      SetWindowLongPtrW(h, GWLP_USERDATA, reinterpret_cast<LONG_PTR>(self));
    }
    return self ? self->message(h, m, w, l) : DefWindowProcW(h, m, w, l);
  }
  Diagnostics diagnostics_;
  Options options_;
  PlayerConfig mapping_;
  PackSettingsStore settings_;
  std::optional<PackSettingsCandidate> activeProfile_;
  std::optional<Pack> activeImportedPack_;
  PackAppearance activeAppearance_;
  json legacyAppearance_ = json::object();
  std::string legacyPackId_;
  HWND window_ = nullptr;
  GpuPreference gpuPreference_ = GpuPreference::Discrete;
  PlayerOutput renderer_;
  unsigned idleWidth_=1920,idleHeight_=720;
  EffekseerBackend effects_;
  Scene scene_;
  std::string pluginFallback_;
  std::string preservedPackId_;
  fs::path activeNativeModule_;
  std::string activeDesktopControl_, desktopError_;
  DesktopWallpaperLayer desktopLayer_;
  bool desktopActive_=false;
  bool activeNativeUsesD3D12_ = false;
  struct MissingWallpaper {
    fs::path module;
    FramePacer::Time retryAt;
    std::chrono::seconds delay{1};
  };
  std::optional<MissingWallpaper> missingWallpaper_;
  bool pluginRecoveryPending_ = false;
  bool pluginPointerDown_ = false, pluginPaused_ = false;
  bool effekseerReady_ = false;
  std::unique_ptr<RawKeyboard> keyboard_;
  IpcServer server_;
  DisplayGuard displayGuard_;
  KeyboardTaskbar keyboardTaskbar_;
  NOTIFYICONDATAW tray_{};
  TrayMenu *activeTrayMenu_ = nullptr;
  bool exitShortcut_ = false, quittingSession_ = false;
  UINT taskbarCreated_ = 0;
  HICON icon_ = nullptr;
  bool hasMonitor_ = false, paused_ = false, visible_ = true, dirty_ = true;
  bool persistedPaused_ = false;
  bool exclusiveDisplay_ = false;
  bool hideKeyboardTaskbar_ = false;
  bool keyboardTaskbarFailed_ = false;
  bool keyboardZOrderTimer_ = false;
  HANDLE temporaryPauseOwner_ = nullptr;
  DWORD temporaryPauseOwnerPid_ = 0;
  int fps_ = 60, effectId_ = -1;
  int lastTriggeredVariant_ = -1;
  std::vector<int> effectIds_;
  std::optional<VariantPlaybackState> effectPlayback_;
  std::uint64_t randomState_ =
      (GetTickCount64() << 16) ^ GetCurrentProcessId() ^ 0x9e3779b97f4a7c15ull;
  bool runtimeControls_ = false;
  uint64_t framesRendered_ = 0;
  uint64_t effectsEvicted_ = 0;
  Dx12PresentCounters dx12PresentCounters_;
  FramePacer pacer_;
  FrameLatencyGuard swapWait_;
  std::optional<FramePacer::Time> deviceRetryAt_, monitorSuspendAt_, outputRetryAt_;
  bool graphicsSuspended_ = false, ignoreSwapWait_ = false;
  std::unique_ptr<PerfStats> perf_;
  bool highResolutionWait_ = false;
  float packScale_ = 1, effectScale_ = 1, frameMs_ = 0;
  float effectBrightness_ = 1;
  float effectSpeed_ = 1, spriteDuration_ = 0;
  float effectIntensity_ = 1;
  std::array<std::uint8_t, 3> effectColor_{255, 255, 255};
  float effectSaturation_ = 1, effectHue_ = 0;
  std::string effectPalette_ = "original";
  std::vector<EffectRgb> effectPaletteColors_ = defaultCustomPalette();
  std::string packId_, monitorDevice_, error_;
  std::optional<std::vector<Monitor>> testMonitors_;
  bool testResizeFailure_ = false;
  std::string storedMonitorDevice_, monitorError_;
  DisplayIdentity monitorIdentity_;
  std::vector<Pack> packs_;
  std::uint64_t catalogRevision_ = 0;
  CatalogScan catalogScan_;
  std::chrono::steady_clock::time_point lastFrame_ = std::chrono::steady_clock::now();
  std::chrono::steady_clock::time_point lastInput_ = std::chrono::steady_clock::now();
};
json Player::status() const {
  const auto input = keyboard_ ? keyboard_->diagnostics() : RawKeyboard::Diagnostics{};
  const auto guardError = options_.headless ? std::string() : displayGuard_.error();
  PROCESS_MEMORY_COUNTERS memory{};
  memory.cb = sizeof(memory);
  GetProcessMemoryInfo(GetCurrentProcess(), &memory, sizeof(memory));
  FILETIME created{}, exited{}, kernel{}, user{};
  GetProcessTimes(GetCurrentProcess(), &created, &exited, &kernel, &user);
  ULARGE_INTEGER k{}, u{};
  k.LowPart = kernel.dwLowDateTime;
  k.HighPart = kernel.dwHighDateTime;
  u.LowPart = user.dwLowDateTime;
  u.HighPart = user.dwHighDateTime;
  json result = {
      {"running", true},
      {"exitShortcutRegistered", exitShortcut_},
      {"playerPid", GetCurrentProcessId()},
      {"nativeWorkerPid", scene_.plugin ? scene_.plugin->workerPid() : 0},
      {"nativeWorkerAbi", scene_.plugin ? scene_.plugin->renderAbi() : 0},
      {"hostD3D11Device", renderer_.device() != nullptr},
      {"hostD3D12Device", renderer_.dx12() && renderer_.dx12()->device()},
      {"dx12Presentation", !renderer_.dx12() ? "none" : renderer_.dx12()->composes() ?
                               "worker-composition" : "shared-copy"},
      {"runtimeRoot", utf8(options_.root.wstring())},
      {"inputElevated", processElevated()},
      {"playerStateDirectory", utf8(options_.stateDir.wstring())},
      {"playerExplicitPipe", options_.explicitPipe ? utf8(options_.pipe) : ""},
      {"headlessHardware", options_.hardwareHeadless},
      {"presentPending", renderer_.presentPending()},
      {"gpuRecovering", deviceRetryAt_.has_value() || outputRetryAt_.has_value()},
      {"graphicsSuspended", graphicsSuspended_},
      {"desktopLayerActive", desktopActive_ && IsWindow(desktopLayer_.get())},
      {"presentsCompleted", renderer_.presentsCompleted()},
      {"presentsBusy", renderer_.presentsBusy()},
      {"lastPresentResult", renderer_.lastPresentResult()},
      {"rawInputRegistered", keyboard_ && keyboard_->registered()},
      {"shortcutHookActive", keyboard_ && keyboard_->shortcutHookActive()},
      {"shortcutDownEvents", input.shortcutDownEvents},
      {"recoveredAltReleases", input.recoveredAltReleases},
      {"selectedKeyboardDevices", keyboard_ ? keyboard_->selectedDeviceCount() : 0},
      {"rawInputPackets", input.packets},
      {"selectedKeyboardPackets", input.selectedPackets},
      {"keyDownEvents", input.downEvents},
      {"keyUpEvents", input.upEvents},
      {"rawInputReadErrors", input.readErrors},
      {"lastRawInputAgeMs",
       input.lastPacketMs ? json(GetTickCount64() - input.lastPacketMs) : json(nullptr)},
      {"paused", paused_},
      {"calibrationPauseOwnerPid", temporaryPauseOwner_ ? json(temporaryPauseOwnerPid_) : json(nullptr)},
      {"visible", visible_},
      {"fps", fps_},
      {"renderFpsLimit", renderFps()},
      {"effectBrightness", effectBrightness_},
      {"backgroundBrightness", activeAppearance_.backgroundBrightness},
      {"backgroundSaturation", activeAppearance_.backgroundSaturation},
      {"backgroundHue", activeAppearance_.backgroundHue},
      {"backgroundColor", activeAppearance_.backgroundColor},
      {"effectScale", effectScale_},
      {"effectIntensity", effectIntensity_},
      {"effectColor", effectColor_},
      {"effectSaturation", effectSaturation_},
      {"effectSpeed", effectSpeed_},
      {"spriteDuration", spriteDuration_},
      {"effectHue", normalizedEffectHue(activePalette(), effectHue_)},
      {"effectPalette", effectPalette_},
      {"effectPaletteColors", effectPaletteColors_},
      {"effectivePaletteColors", activePalette()},
      {"effectVariantCount", effectIds_.size()},
      {"appearanceSupport", (!scene_.empty() || runtimeControls_) ? "full" : "tint_scale"},
      {"packId", packId_},
      {"packSettingsRevision", activeProfile_ ? activeProfile_->revision : 0},
      {"importedRevision", activeImportedPack_ ? activeImportedPack_->importedRevision : ""},
      {"deviceGroup", mapping_.deviceGroup},
      {"calibrationRevision", mapping_.revision},
      {"calibrationError", mapping_.calibrationError.empty() ? json(nullptr)
                                                             : json(mapping_.calibrationError)},
      {"calibratedKeys", mapping_.keys.size()},
      {"monitorDevice", monitorDevice_},
      {"monitorConnected", hasMonitor_},
      {"outputReady", outputReady()},
      {"monitorError", monitorError_.empty() ? json(nullptr) : json(monitorError_)},
      {"exclusiveDisplay", exclusiveDisplay_},
      {"hideKeyboardTaskbar", hideKeyboardTaskbar_},
      {"keyboardTaskbarFailed", keyboardTaskbarFailed_},
      {"gpuPreference", gpuPreference_ == GpuPreference::Integrated ? "integrated" : "discrete"},
      {"gpuAdapter", utf8(renderer_.adapterName())},
      {"gpuAdapterLuid", renderer_.adapterLuid()},
      {"outputWidth", renderer_.width()},
      {"outputHeight", renderer_.height()},
      {"displayGuardActive", !options_.headless && displayGuard_.active()},
      {"displayGuardMode", options_.headless ? std::string("headless") : displayGuard_.mode()},
      {"displayGuardError", guardError.empty() ? json(nullptr) : json(guardError)},
      {"activeEffects", scene_.water ? std::size_t(scene_.water->hasWaves())
                        : scene_.fireworks ? scene_.fireworks->count()
                        : scene_.glass ? scene_.glass->count()
                        : scene_.procedural ? scene_.procedural->count()
                        : scene_.field    ? scene_.field->count()
                        : scene_.keycaps  ? scene_.keycaps->count()
                                    : (scene_.sprite ? scene_.sprite->count() : effects_.ActiveEffectCount())},
      {"activeParticles",
       scene_.fireworks ? scene_.fireworks->particleCount()
       : scene_.procedural ? scene_.procedural->particleCount() : effects_.ActiveParticleCount()},
      {"renderer", !renderer_.hasRenderTarget() ? "none" : renderer_.dx12() ? (scene_.plugin ? "D3D12/NativePlugin" : "D3D12") : scene_.plugin ? "D3D11/NativePlugin"
                   : scene_.water ? "D3D11/WaterSurface"
                   : scene_.fireworks ? "D3D11/Fireworks+HDR"
                   : scene_.glass ? "D3D11/GlassShatter"
                   : scene_.procedural ? (scene_.procedural->isFluid() ? "D3D11/WaterFilm+ParticleFluid"
                                                         : "D3D11/ContinuousBurst")
                   : scene_.field    ? "D3D11/StatefulField+HDR"
                   : scene_.keycaps  ? "D3D11/Keycaps"
                   : scene_.sprite   ? "D3D11/SpriteBurst"
                               : (effectId_ >= 0 ? "D3D11/Effekseer" : "D3D11")},
      {"frameMs", frameMs_},
      {"framesRendered", framesRendered_},
      {"effectsEvicted", effectsEvicted_},
      {"highResolutionWait", highResolutionWait_},
      {"backgroundCopy", renderer_.usingBackdropCopy()},
      {"spriteAtlas", scene_.sprite ? (scene_.sprite->volumeAtlas() ? "3d" : "2d") : "none"},
      {"cpuTimeMs", (k.QuadPart + u.QuadPart) / 10000.0},
      {"memoryMB", memory.WorkingSetSize / (1024.0 * 1024.0)},
      {"error", error_.empty() ? json(nullptr) : json(error_)},
      {"headless", options_.headless},
      {"nativePaintballReady", false}};
  if (options_.dx12PresentStats) {
    const auto &counters = dx12PresentCounters_;
    result["dx12PresentStats"] = {{"drawCalls", counters.drawCalls},
                                  {"presentResults", counters.presentResults},
                                  {"submitted", counters.submitted},
                                  {"busy", counters.busy},
                                  {"occluded", counters.occluded},
                                  {"otherResults", counters.otherResults},
                                  {"retries", counters.retries},
                                  {"retriesWithDraw", counters.retriesWithDraw},
                                  {"idleRetriesWithDraw", counters.idleRetriesWithDraw}};
  }
  return result;
}
json Player::listMonitors(const std::vector<Monitor> &displays) const {
  json list = json::array();
  const auto *selected = resolveMonitor(displays, monitorIdentity_);
  for (const auto &m : displays) {
    int width = m.bounds.right - m.bounds.left, height = m.bounds.bottom - m.bounds.top;
    std::string id = utf8(m.device);
    list.push_back({{"device", id},
                    {"monitorIdentity", utf8(m.identity.path)},
                    {"monitorSerial", utf8(m.identity.serial)},
                    {"label", std::string(m.primary ? "主屏"
                                                    : (width == 1920 && height == 720 ? "键盘屏候选"
                                                                                      : "显示器")) +
                                  " · " + std::to_string(width) + " × " + std::to_string(height)},
                    {"width", width},
                    {"height", height},
                    {"x", m.bounds.left},
                    {"y", m.bounds.top},
                    {"primary", m.primary},
                    {"selected", &m == selected && id == monitorDevice_}});
  }
  return list;
}
json Player::listDevices(bool refresh) {
  const auto devices = refresh && keyboard_ ? keyboard_->refreshDevices()
                                             : RawKeyboard::enumerate(mapping_.deviceGroup);
  if (!devices)
    throw std::runtime_error("无法读取键盘设备，请重试刷新。");
  json list = json::array();
  for (const auto &d : *devices) {
    if (d.path.empty())
      continue;
    std::string path = utf8(d.path);
    std::string folded = path;
    std::transform(folded.begin(), folded.end(), folded.begin(),
                   [](unsigned char c) { return static_cast<char>(std::toupper(c)); });
    size_t vid = folded.find("VID_");
    std::string group = vid == std::string::npos ? path : folded.substr(vid, 17);
    auto existing = std::find_if(list.begin(), list.end(),
                                 [&](const json &item) { return item.at("id") == group; });
    if (existing == list.end()) {
      const bool action75 = group == "VID_1CA6&PID_1A0B";
      const auto label = action75 ? std::string("Action75")
                                  : std::string("其他键盘 ") + std::to_string(list.size() + 1);
      list.push_back({{"id", group}, {"label", label}, {"selected", d.selected}});
    } else if (d.selected)
      (*existing)["selected"] = true;
  }
  std::stable_sort(list.begin(), list.end(), [](const json &a, const json &b) {
    return a["id"] == "VID_1CA6&PID_1A0B" && b["id"] != "VID_1CA6&PID_1A0B";
  });
  return list;
}
json Player::catalogEntry(const Pack &p) const {
  const auto schemaAvailable = settings_.hasSchema(p.id);
  const auto schemaError = settings_.schemaError(p.id);
  return {{"id", p.id},
          {"name", p.name},
          {"kind", p.kind},
          {"supported", p.supported},
          {"description", p.description},
          {"effectVariantCount", p.effects.size()},
          {"runtimeControls", p.runtimeControls},
          {"importUpdatePending", importUpdatePending(p)},
          {"schemaAvailable", schemaAvailable},
          {"schemaVersion", schemaAvailable ? 1 : 0},
          {"schemaError", schemaError.empty() ? json(nullptr) : json(schemaError)},
          {"background", p.background.empty() ? json(nullptr) : json(utf8(p.background.wstring()))},
          {"path", utf8(p.directory.wstring())}};
}
json Player::listPacks(const json &request) {
  json list = json::array();
  const bool paginated = request.contains("offset");
  const auto cursor = paginated ? request.at("offset") : json(0);
  if (!cursor.is_number_integer() || cursor < 0)
    throw std::runtime_error("Invalid wallpaper catalog offset");
  const auto offset = cursor.get<std::uint64_t>();
  if (offset == 0) {
    if (!request.contains("catalogRefreshRevision")) {
      if (!catalogScan_.valid())
        startCatalogScan();
      return {{"pendingCatalogRevision", catalogRevision_ + 1}};
    }
    const auto &refresh = request.at("catalogRefreshRevision");
    if (!refresh.is_number_integer() || refresh <= 0)
      throw std::runtime_error("Invalid wallpaper catalog refresh revision");
    const auto revision = refresh.get<std::uint64_t>();
    if (revision > catalogRevision_) {
      if (revision != catalogRevision_ + 1 || !catalogScan_.valid())
        throw std::runtime_error("Unknown wallpaper catalog refresh");
      if (catalogScan_.wait_for(std::chrono::seconds(0)) != std::future_status::ready)
        return {{"pendingCatalogRevision", revision}};
      // Drop the pending ticket even when the scan threw. The last published
      // list remains valid and a later request can start a new scan.
      auto finished = std::exchange(catalogScan_, {});
      publishCatalog(*finished.get());
    }
  } else if (!request.contains("catalogRevision") ||
             !request.at("catalogRevision").is_number_integer() ||
             request.at("catalogRevision") != catalogRevision_) {
    throw std::runtime_error("Wallpaper catalog changed during pagination");
  }
  if (offset > packs_.size())
    throw std::runtime_error("Invalid wallpaper catalog offset");
  if (!paginated) {
    for (const auto &pack : packs_)
      list.push_back(catalogEntry(pack));
    return list;
  }

  // Budget serialized UTF-8 bytes, including JSON escapes. Leave room for
  // the page cursor, envelope and newline under the 64 KiB transport limit.
  constexpr std::size_t PageBytes = 60 * 1024;
  std::size_t bytes = 0;
  auto next = static_cast<std::size_t>(offset);
  for (; next < packs_.size(); ++next) {
    auto entry = catalogEntry(packs_[next]);
    auto size = entry.dump().size() + 1;
    if (size > PageBytes) {
      entry["description"] = "";
      entry["schemaError"] = nullptr;
      entry["background"] = nullptr;
      size = entry.dump().size() + 1;
      if (size > PageBytes) {
        entry["path"] = nullptr;
        entry["kind"] = "";
        size = entry.dump().size() + 1;
      }
    }
    if (bytes + size > PageBytes)
      break;
    bytes += size;
    list.push_back(std::move(entry));
  }
  return {{"packs", std::move(list)},
          {"catalogRevision", catalogRevision_},
          {"nextOffset", next < packs_.size() ? json(next) : json(nullptr)}};
}
const std::vector<EffectRgb> &Player::activePalette() const {
  return activeAppearance_.resolvedPaletteColors;
}
json Player::listPalettes() const {
  json list = json::array();
  for (const auto &palette : effectPalettePresets())
    list.push_back({{"id", palette.id},
                    {"name", palette.name},
                    {"colors", palette.id == "custom" ? effectPaletteColors_ : palette.colors}});
  return list;
}
void Player::saveSettings() {
  writeJson(options_.stateDir / L"native-player.json", {{"version", 1},
                                                        {"fps", fps_},
                                                        {"packId", preservedPackId_.empty() ? packId_ : preservedPackId_},
                                                        {"monitorDevice", storedMonitorDevice_},
                                                        {"monitorTargetPath", utf8(monitorIdentity_.path)},
                                                        {"monitorTargetSerial", utf8(monitorIdentity_.serial)},
                                                        {"deviceGroup", mapping_.deviceGroup},
                                                        {"paused", persistedPaused_},
                                                        {"visible", visible_},
                                                        {"hideKeyboardTaskbar", hideKeyboardTaskbar_},
                                                        {"exclusiveDisplay", exclusiveDisplay_}});
}
void Player::refreshPacks() {
  auto snapshot = scanWallpaperCatalog(options_.root, options_.stateDir, activeImportedPack_,
                                       settings_.catalogSchemas());
  // A synchronous reload has newer directory contents than any scan it supersedes.
  catalogScan_ = {};
  publishCatalog(snapshot);
}
void Player::startCatalogScan() {
  std::packaged_task<std::shared_ptr<WallpaperCatalogSnapshot>()> task(
      [root = options_.root, state = options_.stateDir, active = activeImportedPack_,
       schemas = settings_.catalogSchemas()] {
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
        return std::make_shared<WallpaperCatalogSnapshot>(scanWallpaperCatalog(root, state, active, schemas));
      });
  auto future = task.get_future().share();
  // The task holds only value copies and can finish without the player or its window.
  std::thread(std::move(task)).detach();
  catalogScan_ = std::move(future);
}
void Player::publishCatalog(WallpaperCatalogSnapshot &snapshot) {
  auto &listed = snapshot.entries;
  std::string rejected;
  const auto alias = std::find_if(listed.begin(), listed.end(), [&](const Pack &pack) {
    return pack.id != packId_ && samePackId(pack.id, packId_);
  });
  if (alias != listed.end()) {
    rejected = alias->id;
    listed.erase(alias);
  }
  const auto active = std::find_if(listed.begin(), listed.end(), [&](const Pack &pack) {
    return pack.id == packId_;
  });
  const bool keepActive = active == listed.end() || importUpdatePending(*active);
  // A retained revision discovered before a wallpaper switch must not be listed
  // as the active recovery option for the newly selected wallpaper.
  if (active == listed.end() && snapshot.retainedActive && activeImportedPack_ &&
      snapshot.retainedActive->id == activeImportedPack_->id &&
      snapshot.retainedActive->directory == activeImportedPack_->directory &&
      snapshot.retainedActive->importedRevision == activeImportedPack_->importedRevision)
    listed.push_back(std::move(*snapshot.retainedActive));
  settings_.adoptCatalog(snapshot.settings, snapshot.unchangedSchemas,
                         keepActive ? packId_ : std::string(), rejected);
  packs_.swap(listed);
  ++catalogRevision_;
}
bool Player::importUpdatePending(const Pack &pack) const {
  return activeImportedPack_ && activeImportedPack_->id == pack.id &&
         !pack.importedRevision.empty() &&
         activeImportedPack_->importedRevision != pack.importedRevision;
}
const Pack &Player::requirePack(const std::string &id) const {
  const auto found =
      std::find_if(packs_.begin(), packs_.end(), [&](const Pack &pack) { return pack.id == id; });
  if (found == packs_.end())
    throw std::runtime_error("Wallpaper is not available");
  return *found;
}
static void validateVariantSlots(const VariantPlaybackRules &rules, std::size_t count) {
  if (std::any_of(rules.order.begin(), rules.order.end(),
                  [count](unsigned slot) { return slot > count; }))
    throw std::runtime_error("A playback variant refers to a file missing from manifest.effects");
}
static void validateEffectSettings(const Pack &pack, const PackAppearance &appearance) {
  if (pack.kind != "effekseer" &&
      std::any_of(appearance.dynamicInputs.begin(), appearance.dynamicInputs.end(),
                  [](const auto &input) { return input.has_value(); }))
    throw std::runtime_error("Dynamic inputs require an Effekseer pack");
  if (!appearance.playback || pack.kind == "fireworks")
    return;
  if (pack.kind != "effekseer" || pack.runtimeControls)
    throw std::runtime_error("Variant playback requires a standard CPU Effekseer pack");
  if (pack.declaredEffectSlots == 0 || pack.declaredEffectSlots != pack.effects.size())
    throw std::runtime_error("Variant playback requires distinct files in manifest.effects");
  validateVariantSlots(*appearance.playback, pack.effects.size());
}
bool Player::validateUniforms(const std::vector<int> &ids, const PackAppearance &appearance,
                              std::string &error) const {
  if (ids.empty() && !appearance.uniforms.empty()) {
    error = "This wallpaper renderer does not expose material uniforms";
    return false;
  }
  std::vector<EffekseerBackend::UniformValue> values;
  for (const auto &uniform : appearance.uniforms)
    values.push_back({uniform.name, uniform.component, uniform.value});
  for (const auto id : ids)
    if (!effects_.ValidateMaterialUniforms(id, values, error))
      return false;
  return true;
}
// A new scene receives its initial values at creation. For an existing scene,
// wait for the worker before publishing the new profile or writing it to disk.
static bool usesDesktopAlpha(const std::string &control,const json &values) {
  return !control.empty() && values.at(control).get<double>() < 100.;
}

bool Player::setDesktopMode(bool transparent,std::string &error) {
  if (options_.importTrial || !usesOutputWindow()) return true;
  auto *output=renderer_.dx12();
  if (transparent) {
    if (!output || !output->composes() || !hasMonitor_) {
      error="当前输出不支持桌面透明显示，请连接键盘屏并启用支持桌面合成的渲染模式。";
      return false;
    }
    HWND progman=FindWindowW(L"Progman",nullptr);
    // Test windows exist only on a private desktop. Never request Explorer there.
    if (!options_.headless && (!GetShellWindow() ||
        shellWindowPid(progman)!=shellWindowPid(GetShellWindow()))) progman=nullptr;
    if (!desktopLayer_.place(window_,progman,!options_.headless,error,
                             [&](HWND target) {return output->retargetComposition(target,error);})) {
      if (!desktopActive_) desktopLayer_.reset();
      return false;
    }
  } else if (desktopActive_) {
    if (output && output->composes() && !output->retargetComposition(window_,error)) return false;
    desktopLayer_.reset();
  }
  if (!desktopError_.empty() && error_==desktopError_) error_.clear();
  desktopError_.clear();
  desktopActive_=transparent;
  return true;
}

void Player::refreshDesktopLayer() {
  if (!desktopActive_ || !hasMonitor_) return;
  std::string failure;
  if (!setDesktopMode(true,failure)) {
    error_=desktopError_=failure;
    // Explorer can restart independently. Keep the top-level control window alive and hidden.
    desktopLayer_.show(false);
    SetTimer(window_,MonitorTopologyTimer,1000,nullptr);
    return;
  }
  desktopLayer_.show(visible_ && outputReady() && !packId_.empty());
}

void Player::updateProfile(PackSettingsCandidate &profile, const PackAppearance &appearance,
                           std::optional<bool> persist) {
  const bool active = profile.id == packId_;
  if (active && pluginRecoveryPending_)
    throw std::runtime_error(error_);
  const bool notify = active && scene_.plugin && activeProfile_->values != profile.values;
  const bool wasDesktop=desktopActive_;
  const bool changeDesktop=active && !graphicsSuspended_ && !activeDesktopControl_.empty() &&
      wasDesktop!=usesDesktopAlpha(activeDesktopControl_,profile.values);
  std::string failure;
  if (changeDesktop && !setDesktopMode(!wasDesktop,failure))
    throw std::runtime_error(failure);
  try {
    if (notify && !sendPluginEvent(A75_SETTINGS, 0, 0, "", profile.values.dump().c_str()))
      throw std::runtime_error(error_);
    if (persist.has_value()) {
      settings_.commit(profile, *persist);
      profile.stored = *persist;
    }
  } catch (...) {
    if (changeDesktop) {
      if (!setDesktopMode(wasDesktop,failure)) error_=failure;
      updateVisibility();
    }
    if (notify && scene_.plugin)
      sendPluginEvent(A75_SETTINGS, 0, 0, "", activeProfile_->values.dump().c_str());
    throw;
  }
  if (changeDesktop && usesOutputWindow()) updateVisibility();
  if (active && (!activeProfile_ || activeProfile_->revision != profile.revision ||
                 activeProfile_->values != profile.values))
    applyProfile(profile, appearance);
}
void Player::applyProfile(const PackSettingsCandidate &profile, const PackAppearance &appearance) {
  activeProfile_ = profile;
  activeAppearance_ = appearance;
  if (!effectIds_.empty() && appearance.playback) {
    if (!effectPlayback_)
      effectPlayback_.emplace();
    effectPlayback_->configure(*appearance.playback);
  } else {
    effectPlayback_.reset();
  }
  effectBrightness_ = appearance.brightness;
  effectScale_ = appearance.scale;
  effectIntensity_ = appearance.intensity;
  effectSaturation_ = appearance.saturation;
  effectHue_ = 0;
  effectColor_ = appearance.color;
  effectPalette_ = appearance.palette;
  effectPaletteColors_ = appearance.paletteColors;
  effectSpeed_ = appearance.speed;
  spriteDuration_ = appearance.spriteDuration;
  applyAppearance();
  dirty_ = true;
}
bool Player::resizeOutput(unsigned width, unsigned height, std::string &error) {
  if (std::exchange(testResizeFailure_, false)) {
    error = "Diagnostic output resize failed";
    return false;
  }
  if (!renderer_.device() && !renderer_.dx12()) {
    error = "Graphics output is unavailable";
    return false;
  }
  if (!renderer_.resize(width, height, error))
    return false;
  dx12PresentCounters_.discardPending();
  effects_.Resize(width, height);
  if (scene_.plugin) {
    try {
      const auto &pack = activeImportedPack_ ? *activeImportedPack_ : requirePack(packId_);
      if (activatePack(&pack, error_, true))
        return true;
    } catch (const std::exception &failure) {
      error_ = failure.what();
    }
    pluginFailed();
  }
  return true;
}
void Player::chooseMonitor(bool persistBinding, const std::vector<Monitor> &list) {
  const auto *chosen = resolveMonitor(list, monitorIdentity_, storedMonitorDevice_.empty());
  RECT previousBounds{};
  const bool sameOutput = chosen && hasMonitor_ && monitorDevice_ == utf8(chosen->device) &&
                          GetWindowRect(window_, &previousBounds) &&
                          EqualRect(&previousBounds, &chosen->bounds);
  if (!sameOutput)
    resetPluginPointer();
  hasMonitor_ = chosen != nullptr;
  monitorDevice_.clear();
  monitorError_.clear();
  if (chosen) {
    monitorDevice_ = utf8(chosen->device);
    const bool bindingChanged = storedMonitorDevice_ != monitorDevice_ ||
                                monitorIdentity_.path != chosen->identity.path ||
                                (!chosen->identity.serial.empty() &&
                                 monitorIdentity_.serial != chosen->identity.serial);
    storedMonitorDevice_ = monitorDevice_;
    monitorIdentity_.path = chosen->identity.path;
    if (!chosen->identity.serial.empty())
      monitorIdentity_.serial = chosen->identity.serial;
    if (bindingChanged && persistBinding && !offscreenOnly()) {
      try {
        saveSettings();
      } catch (const std::exception &exception) {
        error_ = std::string("Display binding could not be saved: ") + exception.what();
      }
    }
  } else if (!options_.headless) {
    monitorError_ = monitorIdentity_.empty() && !storedMonitorDevice_.empty()
                        ? "请重新选择键盘屏以确认原有显示设置。"
                        : "无法确认键盘屏，请重新连接或选择显示器。";
  }
  if (!hasMonitor_ || offscreenOnly()) {
    outputRetryAt_.reset();
    if (!offscreenOnly()) {
      deviceRetryAt_.reset();
      if (!graphicsSuspended_ && !monitorSuspendAt_)
        monitorSuspendAt_ = FramePacer::Clock::now() + MonitorDisconnectGrace;
      stopEffects();
    }
    updateVisibility();
    return;
  }
  if (monitorSuspendAt_ && (pluginRecoveryPending_ || gpuDeviceLost()))
    graphicsSuspended_ = true;
  monitorSuspendAt_.reset();
  RECT r = chosen->bounds;
  unsigned width = r.right - r.left, height = r.bottom - r.top;
  const auto retryOutput = [&](const std::string &error) {
    const bool alreadyRetrying = outputRetryAt_.has_value();
    outputRetryAt_ = FramePacer::Clock::now() +
                     std::chrono::milliseconds(alreadyRetrying ? 1000 : 250);
    monitorError_ = "无法在键盘屏上建立输出，正在重试。";
    if (!alreadyRetrying) {
      diagnostics_.write("WARN", error);
      stopEffects();
    }
    updateVisibility();
  };
  if (usesOutputWindow() && !sameOutput &&
      !SetWindowPos(window_, nullptr, r.left, r.top, width, height, SWP_NOACTIVATE | SWP_NOZORDER)) {
    retryOutput("Place keyboard screen window failed: " + std::to_string(GetLastError()));
    return;
  }
  std::string resizeError;
  if (!graphicsSuspended_ && (renderer_.device() || renderer_.dx12()) &&
      (outputRetryAt_ || !renderer_.hasRenderTarget() ||
       renderer_.width() != width || renderer_.height() != height) &&
      !resizeOutput(width, height, resizeError)) {
    retryOutput(resizeError);
    return;
  }
  outputRetryAt_.reset();
  if (graphicsSuspended_ && !deviceRetryAt_ && !packId_.empty()) {
    // Rebuild on the window thread before dispatching queued keyboard input.
    deviceRetryAt_ = FramePacer::Clock::now();
    recoverGraphics();
    return;
  }
  if (packId_.empty())
    graphicsSuspended_ = false;
  dirty_ = true;
  updateVisibility();
  refreshDesktopLayer();
}
void Player::updateVisibility() {
  const bool suspended = paused_ || !visible_ || (!outputReady() && !offscreenOnly());
  renderer_.setBackgroundPaused(suspended);
  if (scene_.plugin && suspended != pluginPaused_) {
    pluginPaused_ = suspended;
    sendPluginEvent(suspended ? A75_PAUSE : A75_RESUME);
  }
  if (!pluginPointerEnabled())
    resetPluginPointer();
  if (desktopActive_) desktopLayer_.show(visible_ && outputReady() && !packId_.empty());
  if (window_ && options_.testWindow != TestWindow::None) {
    const bool showing = !desktopActive_ && visible_ && outputReady() && !packId_.empty();
    if (showing != (IsWindowVisible(window_) != FALSE))
      ShowWindow(window_, showing ? SW_SHOWNOACTIVATE : SW_HIDE);
  }
  if (window_ && !options_.headless) {
    const bool showing = !desktopActive_ && visible_ && outputReady() && !packId_.empty();
    const auto style = GetWindowLongPtrW(window_, GWL_EXSTYLE);
    const bool pointerInteraction = pluginPointerEnabled() || canClickScene();
    const auto pointerStyle = pointerInteraction ? style & ~WS_EX_TRANSPARENT
                                                : style | WS_EX_TRANSPARENT;
    if (style != pointerStyle)
      SetWindowLongPtrW(window_, GWL_EXSTYLE, pointerStyle);
    if (showing != (IsWindowVisible(window_) != FALSE))
      ShowWindow(window_, showing ? SW_SHOWNOACTIVATE : SW_HIDE);
    if (showing) {
      keepKeyboardAboveTaskbar(window_);
      if (!keyboardZOrderTimer_)
        keyboardZOrderTimer_ = SetTimer(window_, KeyboardZOrderTimer, 400, nullptr) != 0;
    } else if (keyboardZOrderTimer_) {
      KillTimer(window_, KeyboardZOrderTimer);
      keyboardZOrderTimer_ = false;
    }
  }
  refreshDisplayGuard();
  refreshKeyboardTaskbar();
  dirty_ = true;
}
void Player::refreshDisplayGuard() {
  // Tests must never create the guard worker or install real desktop
  // hooks/clips.
  if (options_.headless || quittingSession_)
    return;
  // Keep the requested target when disconnected so the guard can report its
  // fail-open reason instead of silently turning a requested guard into
  // disabled. A normal pause keeps the static wallpaper protected; calibration
  // suspends it.
  displayGuard_.configure(window_, wide(monitorDevice_), monitorIdentity_,
                          exclusiveDisplay_ && visible_ && !packId_.empty(),
                          temporaryPauseOwner_ != nullptr || (hasMonitor_ && !outputReady()));
}
void Player::refreshKeyboardTaskbar() {
  RECT bounds{};
  if (hideKeyboardTaskbar_ && !quittingSession_ && usesOutputWindow() && visible_ &&
      outputReady() && !packId_.empty() && GetWindowRect(window_, &bounds)) {
    keyboardTaskbarFailed_ = !keyboardTaskbar_.hide(window_, bounds);
  } else {
    keyboardTaskbar_.restore();
    keyboardTaskbarFailed_ = false;
  }
}
void Player::stopEffects() {
  resetPluginPointer(false);
  if (scene_.plugin)
    sendPluginEvent(A75_INPUT_RESET);
  if (scene_.water)
    scene_.water->clear();
  if (scene_.fireworks)
    scene_.fireworks->clear();
  if (scene_.glass)
    scene_.glass->clear();
  if (scene_.procedural)
    scene_.procedural->clear();
  if (scene_.field)
    scene_.field->clear();
  if (scene_.keycaps)
    scene_.keycaps->clear();
  if (scene_.sprite)
    scene_.sprite->clear();
  if (effectPlayback_)
    effectPlayback_->rewind();
  effects_.StopAll();
  dirty_ = true;
}
void Player::applyAppearance() {
  scene_.configure(activeAppearance_);
  renderer_.setMediaPlacement(activeAppearance_.backgroundFit,activeAppearance_.backgroundZoom,
                             activeAppearance_.backgroundPanX,activeAppearance_.backgroundPanY);
  renderer_.setBackgroundAppearance(
      activeAppearance_.backgroundBrightness, activeAppearance_.backgroundSaturation,
      activeAppearance_.backgroundHue, activeAppearance_.backgroundColor, activeAppearance_.backgroundStyle==1);
  if (scene_.sprite) {
    lastTriggeredVariant_ = -1;
    scene_.sprite->setBrightness(effectBrightness_);
    scene_.sprite->setScale(effectScale_);
    scene_.sprite->setIntensity(effectIntensity_);
    scene_.sprite->setColor(effectColor_);
    scene_.sprite->setDuration(spriteDuration_);
    scene_.sprite->setSpeed(effectSpeed_);
    scene_.sprite->setColorGrade(effectSaturation_, effectHue_, activePalette(),
                           activeAppearance_.paletteOverride);
  }
  effects_.SetAppearance(effectScale_, effectBrightness_, effectIntensity_, effectColor_,
                         effectSpeed_);
  effects_.SetDynamicInputs(activeAppearance_.dynamicInputs);
  effects_.SetColorGrade(effectSaturation_, effectHue_, activePalette(),
                         activeAppearance_.paletteOverride);
  std::vector<EffekseerBackend::UniformValue> uniforms;
  for (const auto &value : activeAppearance_.uniforms)
    uniforms.push_back({value.name, value.component, value.value});
  for (const auto id : effectIds_) {
    std::string error;
    if (!effects_.SetMaterialUniforms(id, uniforms, error))
      error_ = error;
  }
  if (effectIntensity_ == 0)
    stopEffects();
}
bool Player::selectPack(const std::string &id, std::string &error) {
  if (id.empty()) {
    stopEffects();
    if (renderer_.hasRenderTarget()) {
      idleWidth_ = renderer_.width();
      idleHeight_ = renderer_.height();
    }
    releaseGraphics();
    deviceRetryAt_.reset();
    runtimeControls_ = false;
    packId_.clear();
    activeNativeModule_.clear();
    activeDesktopControl_.clear();
    activeNativeUsesD3D12_ = false;
    activeProfile_.reset();
    activeImportedPack_.reset();
    activeAppearance_ = PackAppearance{};
    updateVisibility();
    return true;
  }
  auto found =
      std::find_if(packs_.begin(), packs_.end(), [&](const auto &p) { return p.id == id; });
  if (found == packs_.end()) {
    error = "Wallpaper pack is unavailable or not supported";
    return false;
  }
  return selectCandidate(*found, error);
}
bool Player::selectCandidate(const Pack &pack, std::string &error) {
  const auto &revision = pack.importedRevision;
  const bool recordPlayback = !options_.importTrial && !revision.empty();
  ImportedPlayback playback;
  if (recordPlayback) {
    try {
      playback = readImportedPlayback(options_.stateDir, pack.id);
    } catch (const std::exception &failure) {
      diagnostics_.write("WARN", failure.what());
    }
    if (!playback.lastSuccessful.empty() && playback.lastSuccessful != revision &&
        playback.attempted != revision) {
      // A restart can recover even if the new version exits the process during activation.
      playback.attempted = revision;
      try {
        writeImportedPlayback(options_.stateDir, pack.id, playback);
      } catch (const std::exception &failure) {
        error = std::string("Could not save wallpaper update recovery information. ") + failure.what();
        return false;
      }
    }
  }
  if (!pack.supported) {
    error = "Wallpaper pack is unavailable or not supported";
    return false;
  }
  if (!revision.empty())
    settings_.registerPack(pack.id, pack.directory, pack.settings, pack.kind);
  // An explicit switch still checks the candidate before committing its profile
  // or revision. The temporary target needs no swap chain while the screen is absent.
  const bool temporaryGraphics = graphicsSuspended_;
  bool activated = false;
  try {
    if (!temporaryGraphics || initializeOutput(pack.usesD3D12,1920,720,true,error))
      activated = activatePack(&pack, error);
  } catch (const std::exception &failure) {
    error = failure.what();
  }
  if (temporaryGraphics)
    releaseGraphics();
  if (!activated) {
    if (importUpdatePending(pack))
      settings_.registerPack(pack.id, activeImportedPack_->directory, activeImportedPack_->settings, activeImportedPack_->kind);
    return false;
  }
  if (revision.empty())
    activeImportedPack_.reset();
  else
    activeImportedPack_ = pack;
  if (recordPlayback && playback.lastSuccessful != revision) {
    try {
      writeImportedPlayback(options_.stateDir, pack.id, {revision, {}});
    } catch (const std::exception &failure) {
      diagnostics_.write("WARN", std::string("Wallpaper applied, but the version was not saved. ") +
                                     failure.what());
    }
  }
  return true;
}
bool Player::restoreSelection(const std::string &id, std::string &error) {
  ImportedPlayback playback;
  try {
    playback = readImportedPlayback(options_.stateDir, id);
  } catch (const std::exception &failure) {
    diagnostics_.write("WARN", failure.what());
  }
  const auto published = std::find_if(packs_.begin(), packs_.end(),
                                      [&](const Pack &pack) { return pack.id == id; });
  const auto publishedRevision = published == packs_.end() ? std::string{} : published->importedRevision;
  const bool previousAttempt = !playback.attempted.empty() && playback.attempted == publishedRevision;
  std::string publishedError;
  if (previousAttempt)
    publishedError = "The previous wallpaper update attempt did not finish successfully";
  else if (selectPack(id, publishedError))
    return true;

  error = publishedError;
  if (playback.lastSuccessful.empty() || playback.lastSuccessful == publishedRevision)
    return false;
  const ImportedRevision fallback{id, playback.lastSuccessful};
  const auto retained = packs(options_.root, &fallback);
  if (retained.empty()) {
    error += ". The last successful wallpaper version is unavailable.";
    return false;
  }
  std::string fallbackError;
  if (!selectCandidate(retained.front(), fallbackError)) {
    error += ". Could not restore the last successful wallpaper version. " + fallbackError;
    return false;
  }
  refreshPacks();
  error = "已恢复上一次成功播放的壁纸版本。" + publishedError;
  return true;
}
json Player::protectedImportRevisions(const std::string &id) const {
  json revisions = json::array();
  const auto keep = [&](const std::string &revision) {
    if (!revision.empty() &&
        std::find(revisions.begin(), revisions.end(), revision) == revisions.end())
      revisions.push_back(revision);
  };
  keep(readImportedPlayback(options_.stateDir, id).lastSuccessful);
  if (activeImportedPack_ && activeImportedPack_->id == id)
    keep(activeImportedPack_->importedRevision);
  for (const auto &pack : packs_)
    if (pack.id == id)
      keep(pack.importedRevision);
  return revisions;
}
const Pack *Player::selectedPack() const {
  if (activeImportedPack_)
    return &*activeImportedPack_;
  auto found = std::find_if(packs_.begin(), packs_.end(),
                            [&](const Pack &pack) { return pack.id == packId_; });
  return found == packs_.end() ? nullptr : &*found;
}
bool Player::initializeOutput(bool use12, unsigned width, unsigned height, bool temporary,
                              std::string &error, bool allowComposition) {
  const bool offscreen = temporary || !usesOutputWindow() || options_.importTrial;
  const bool hardware = temporary || options_.hardwareHeadless || (options_.importTrial && use12);
  if (!renderer_.initialize(window_, width, height, offscreen, error, hardware, gpuPreference_,
                            use12, allowComposition && options_.testWindow != TestWindow::Shared))
    return false;
  diagnostics_.write("INFO", std::string(renderer_.dx12() ? "D3D12 adapter " : "D3D11 adapter ") +
                                utf8(renderer_.adapterName()));
  renderer_.allowBackdropCopy(options_.backdropCopy);
  if (options_.perfStats && options_.headless && !use12) {
    auto stats = std::make_unique<PerfStats>();
    if (stats->initialize(renderer_.device()))
      perf_ = std::move(stats);
  }
  return true;
}
bool Player::activatePack(const Pack *found, std::string &error, bool restoringDevice) {
  const auto activate = [&] {
    if (activateOnCurrentOutput(found, error, restoringDevice))
      return true;
    const auto *output = renderer_.dx12();
    if (!output || !output->composes() || scene_.plugin || gpuDeviceLost())
      return false;
    // The worker or driver may decline composition. Before a scene has been
    // accepted, use the same D3D12 pack on the existing shared-copy route.
    const auto w = renderer_.width(), h = renderer_.height();
    diagnostics_.write("WARN", "D3D12 composition unavailable: " + error);
    error.clear();
    return initializeOutput(true, w, h, graphicsSuspended_, error, false) &&
           activateOnCurrentOutput(found, error, restoringDevice);
  };
  if (!renderer_.hasRenderTarget() && !renderer_.device() && !renderer_.dx12() && packId_.empty()) {
    unsigned w = idleWidth_, h = idleHeight_;
    RECT rect{};
    if (usesOutputWindow() && GetClientRect(window_, &rect) && rect.right > 0 && rect.bottom > 0) {
      w = static_cast<unsigned>(rect.right);
      h = static_cast<unsigned>(rect.bottom);
    }
    if (!initializeOutput(found->usesD3D12, w, h, graphicsSuspended_, error))
      return false;
    if (activate())
      return true;
    releaseGraphics();
    return false;
  }
  const bool previous12 = renderer_.dx12() != nullptr;
  if (found->usesD3D12 == previous12)
    return activate();
  if (gpuDeviceLost() || !renderer_.hasRenderTarget() ||
      (deviceRetryAt_ && !restoringDevice && !graphicsSuspended_)) {
    error = "Graphics output is unavailable";
    return false;
  }
  const auto w = renderer_.width(), h = renderer_.height();
  const auto *previous = selectedPack();
  // DXGI permits only one flip chain per HWND. Restore the last accepted pack
  // and profile if replacement device creation or first-frame validation fails.
  releaseGraphics();
  try {
    if (initializeOutput(found->usesD3D12, w, h, graphicsSuspended_, error) &&
        activate())
      return true;
  } catch (const std::exception &failure) {
    error = failure.what();
  }
  releaseGraphics();
  std::string rollback;
  if (!initializeOutput(previous12, w, h, graphicsSuspended_, rollback) ||
      (previous && activeProfile_ && !activateOnCurrentOutput(previous, rollback, true))) {
    error += ". Could not restore previous output: " + rollback;
    scheduleDeviceRecovery(error);
  }
  return false;
}
bool Player::activateOnCurrentOutput(const Pack *found, std::string &error, bool restoringDevice) {
  if (gpuDeviceLost()) {
    error = "Graphics device was lost";
    scheduleDeviceRecovery(error);
    return false;
  }
  if (!renderer_.hasRenderTarget() || (deviceRetryAt_ && !restoringDevice && !graphicsSuspended_)) {
    error = "Graphics output is unavailable";
    return false;
  }
  const auto &id = found->id;
  PackSettingsCandidate profile;
  PackAppearance appearance;
  try {
    profile = restoringDevice ? activeProfile_.value() : settings_.current(id);
    if (id == legacyPackId_ && !profile.stored && !legacyAppearance_.empty()) {
      auto legacy = legacyAppearance_;
      if (found->kind == "spriteBurst") {
        legacy["sprite.duration"] = found->sprite.duration;
        const double brightness = legacy.value("effectBrightness", 1.0),
                     intensity = legacy.value("effectIntensity", 1.0);
        if (std::isfinite(brightness) && std::isfinite(intensity)) {
          const double product = brightness * intensity;
          legacy["effectBrightness"] = std::clamp(product, 0.0, 4.0);
          legacy["effectIntensity"] = 1;
          if (product > 4)
            error_ = "The previous combined glow exceeded the new brightness "
                     "range and was limited to its maximum";
        }
      }
      profile = settings_.migrate(id, legacy);
    }
    appearance = restoringDevice ? activeAppearance_ : settings_.evaluate(profile);
    validateEffectSettings(*found, appearance);
  } catch (const std::exception &exception) {
    error = exception.what();
    return false;
  }
  std::vector<int> candidates;
  Scene candidate;
  bool createdBackend = false;
  auto discardCandidates = [&] {
    if (createdBackend) {
      effects_.Shutdown();
      effekseerReady_ = false;
    } else
      for (const auto effect : candidates)
        if (std::find(effectIds_.begin(), effectIds_.end(), effect) == effectIds_.end())
          effects_.UnloadEffect(effect);
  };
  if (!candidate.load(*found, renderer_, options_, profile.values, error))
    return false;
  if (found->kind == "effekseer") {
    if (!effekseerReady_) {
      if (!effects_.Initialize(renderer_.device(), renderer_.context())) {
        error = effects_.LastError();
        return false;
      }
      effekseerReady_ = createdBackend = true;
      effects_.Resize(renderer_.width(), renderer_.height());
    }
    effects_.SetAppearance(effectScale_, effectBrightness_, effectIntensity_, effectColor_,
                           effectSpeed_);
    fs::path package;
    if (options_.importTrial || (appearance.playback && found->importedRevision.empty()))
      package = found->directory;
    else if (!found->importedRevision.empty())
      package = found->directory / L".action75-versions" / fs::u8path(found->importedRevision);
    for (std::size_t index = 0; index < found->effects.size(); ++index) {
      const int effect = effects_.LoadEffect(found->effects[index], found->runtimeControls, index,
                                             package);
      if (effect < 0) {
        error = effects_.LastError();
        discardCandidates();
        return false;
      }
      candidates.push_back(effect);
    }
  }
  if (!validateUniforms(candidates, appearance, error)) {
    discardCandidates();
    return false;
  }
  auto oldBackground = renderer_.takeBackground();
  const auto prepareFirstFrame = [&] {
    const bool backgroundLoaded = renderer_.dx12() || (
        !found->canvasEffect.empty()
            ? renderer_.loadCanvasBackground(found->canvasEffect, error)
            : found->background.empty() ||
                  renderer_.loadBackground(found->background, error,
                                           candidate.sprite ? 4194304u : 33554432u, found->kind=="image" || found->kind=="video"));
    if (!backgroundLoaded)
      return false;
    candidate.configure(appearance);
    renderer_.setMediaPlacement(appearance.backgroundFit,appearance.backgroundZoom,
                               appearance.backgroundPanX,appearance.backgroundPanY);
    renderer_.setBackgroundAppearance(appearance.backgroundBrightness, appearance.backgroundSaturation,
                                      appearance.backgroundHue, appearance.backgroundColor, appearance.backgroundStyle==1);
    return renderer_.validateFrame([&] {
      if (!candidate.draw(renderer_, clockSeconds(FramePacer::Clock::now()), paused_, error))
        return false;
      if (auto backgroundError = renderer_.backgroundError(); !backgroundError.empty()) {
        error = std::move(backgroundError);
        return false;
      }
      return true;
    }, error);
  };
  bool ready = false;
  const bool wasDesktop=desktopActive_;
  try {
    if (prepareFirstFrame() && setDesktopMode(usesDesktopAlpha(found->desktopOpacityControl,profile.values),error)) {
      if (!restoringDevice) {
        settings_.commit(profile, !options_.importTrial);
        profile.stored = !options_.importTrial;
      }
      ready = true;
    }
  } catch (const std::exception &exception) {
    error = exception.what();
  }
  if (!ready) {
    std::string rollback;
    if (desktopActive_!=wasDesktop) {
      if (!setDesktopMode(wasDesktop,rollback)) error+="; "+rollback;
      updateVisibility();
    }
    if (scene_.compositionSurface && renderer_.dx12())
      renderer_.dx12()->showComposition(scene_.compositionSurface,rollback);
    renderer_.restoreBackground(std::move(oldBackground));
    discardCandidates();
    if (gpuDeviceLost())
      scheduleDeviceRecovery(error);
    return false;
  }
  stopEffects();
  if (candidates.empty() && effekseerReady_) {
    effects_.Shutdown();
    effekseerReady_ = false;
  } else {
    for (const auto old : effectIds_)
      if (std::find(candidates.begin(), candidates.end(), old) == candidates.end())
        effects_.UnloadEffect(old);
  }
  effectIds_ = std::move(candidates);
  effectPlayback_.reset();
  lastTriggeredVariant_ = -1;
  effectId_ = effectIds_.empty() ? -1 : effectIds_.front();
  pluginRecoveryPending_ = false;
  if (candidate.plugin && packId_ != id)
    pluginFallback_ = packId_;
  scene_ = std::move(candidate);
  dx12PresentCounters_.discardPending();
  activeNativeModule_ = found->nativeModule;
  activeDesktopControl_=found->desktopOpacityControl;
  activeNativeUsesD3D12_ = found->usesD3D12;
  pluginPaused_ = false;
  runtimeControls_ = found->runtimeControls;
  if (effekseerReady_)
    effects_.SetBackground(renderer_.background());
  packScale_ = found->scale;
  packId_ = id;
  lastInput_ = std::chrono::steady_clock::now();
  applyProfile(profile, appearance);
  if (id == legacyPackId_) {
    legacyPackId_.clear();
    legacyAppearance_ = json::object();
  }
  updateVisibility();
  dirty_ = true;
  return true;
}
int Player::handleKey(const KeyEvent &event) {
  if (paused_ || !visible_ || !outputReady())
    return -1;
  if (event.down)
    return trigger(event.x, event.y, event.code);
  return scene_.plugin && sendPluginEvent(A75_KEY_UP, event.x, event.y, event.code.c_str()) ? 1 : -1;
}

int Player::trigger(float x, float y, const std::string &code) {
  if (scene_.plugin)
    return sendPluginEvent(A75_KEY_DOWN, x, y, code.c_str()) ? 1 : -1;
  if (deviceRetryAt_ || effectIntensity_ == 0)
    return -1;
  if (scene_.water) {
    const bool moving = hasMotion();
    if (!scene_.water->push({x, y}))
      return -1;
    if (!moving)
      dirty_ = true;
    return 1;
  }
  const auto now = std::chrono::steady_clock::now();
  if (scene_.glass) {
    const bool moving = hasMotion();
    if (!scene_.glass->push(clockSeconds(now), x, y))
      return -1;
    if (!moving)
      dirty_ = true;
    return 1;
  }
  if (scene_.fireworks) {
    if (!code.empty() && !scene_.fireworks->readyForKeyBurst())
      return SkippedTrigger;
    const bool moving = hasMotion();
    if (!scene_.fireworks->push(clockSeconds(now), x, y))
      return -1;
    lastInput_ = now;
    if (!moving)
      dirty_ = true;
    return 1;
  }
  if (scene_.procedural) {
    const bool moving = hasMotion();
    if (!scene_.procedural->push(clockSeconds(now), x, y))
      return -1;
    lastInput_ = now;
    if (!moving)
      dirty_ = true;
    return 1;
  }
  if (scene_.field) {
    const bool moving = hasMotion();
    if (!scene_.field->push(clockSeconds(now), x, y))
      return -1;
    lastInput_ = now;
    if (!moving)
      dirty_ = true;
    return 1;
  }
  if (scene_.keycaps) {
    const bool moving = hasMotion();
    if (!scene_.keycaps->push(code, clockSeconds(now)))
      return -1;
    lastInput_ = now;
    if (!moving)
      dirty_ = true;
    return 1;
  }
  if (scene_.sprite) {
    lastTriggeredVariant_ = -1;
    const bool wasActive = scene_.sprite->count() > 0;
    if (!scene_.sprite->push(clockSeconds(now), x, y))
      return -1;
    if (!wasActive) {
      lastFrame_ = now;
      dirty_ = true;
    }
    return 1;
  }
  if (effectId_ < 0 || effectIds_.empty())
    return -1;
  const auto randomIndex = [this](std::size_t count) {
    randomState_ ^= randomState_ >> 12;
    randomState_ ^= randomState_ << 25;
    randomState_ ^= randomState_ >> 27;
    return static_cast<std::size_t>((randomState_ * 2685821657736338717ull) % count);
  };
  std::optional<std::size_t> variantIndex;
  if (effectPlayback_) {
    if (!code.empty() && !effectPlayback_->readyForKeyBurst())
      return SkippedTrigger;
    const unsigned slot = effectPlayback_->choose(randomIndex);
    if (!slot || slot > effectIds_.size())
      return -1;
    variantIndex = slot - 1;
  }
  const bool wasIdle = effects_.CatchUpIdle();
  if (effects_.ActiveEffectCount() >= 64) {
    if (!effects_.StopOldest())
      return -1;
    ++effectsEvicted_;
  }
  const bool wasActive = !wasIdle && effects_.HasActiveEffects();
  const auto pick = variantIndex ? *variantIndex : randomIndex(effectIds_.size());
  lastTriggeredVariant_ = static_cast<int>(pick);
  const int handle = effects_.PlayAt(effectIds_[static_cast<std::size_t>(pick)],
                                     x * renderer_.width(), y * renderer_.height(), packScale_);
  if (handle >= 0 && !wasActive) {
    lastFrame_ = wasIdle ? std::chrono::steady_clock::now() : now;
    dirty_ = true;
  }
  return handle;
}
json Player::dispatch(const json &request) {
  try {
    std::string action = request.at("action").get<std::string>();
    if (request.contains("expectedPlayerPid") &&
        request.at("expectedPlayerPid").get<std::uint64_t>() != GetCurrentProcessId())
      throw std::runtime_error(action == "stop" ? "Player identity changed; stop was not applied"
                                                : "Player identity changed");
    if (request.contains("expectedRuntimeRoot") &&
        !matchesRuntimeRoot(fs::u8path(request.at("expectedRuntimeRoot").get<std::string>()),
                            options_.rootEndpoint))
      throw std::runtime_error(action == "stop" ? "Player directory changed; stop was not applied"
                                                : "Player directory changed");
    json result;
    if (action != "status" && action != "perfRead" && action != "perfReset" &&
        action != "getPackSettings" && action != "previewPackSettings" && action != "listMonitors" &&
        action != "listPacks" && action != "testTrigger")
      diagnostics_.write("INFO", action);
    if (action == "suspendDiagnostics") {
      diagnostics_.suspend(true);
      result = status();
    } else if (action == "resumeDiagnostics") {
      diagnostics_.suspend(false);
      result = status();
    } else if (action == "reloadDirectories") {
      const auto previous = packs_;
      const auto selected = packId_;
      refreshPacks();
      std::string error;
      if (!selected.empty() && !selectPack(selected, error)) {
        packs_ = previous;
        for (const auto &pack : packs_) {
          const auto &loaded = importUpdatePending(pack) ? *activeImportedPack_ : pack;
          settings_.registerPack(pack.id, loaded.directory, loaded.settings, loaded.kind);
        }
        throw std::runtime_error(error);
      }
      result = status();
    } else if (action == "status")
      result = status();
    else if (action == "testMonitor" && options_.headless) {
      // Exercise monitor selection and its timer with a real offscreen device.
      // The optional key is delivered before the main loop can retry recovery.
      const auto name = wide(request.at("device").get<std::string>());
      const auto width = request.value("width", 1920L), height = request.value("height", 720L);
      if (width < 1 || width > 4096 || height < 1 || height > 2160)
        throw std::runtime_error("Invalid diagnostic monitor size");
      testResizeFailure_ = request.value("failResizeOnce", false);
      testMonitors_.emplace();
      if (!name.empty()) {
        const RECT bounds{0, 0, width, height};
        testMonitors_->push_back({name, bounds, bounds, false, {name, L"test-keyboard"}});
      }
      chooseMonitor(true);
      result = status();
      if (request.value("firstKey", false)) {
        const auto *key = mapping_.findCode("Space");
        if (!key)
          throw std::runtime_error("Missing diagnostic key mapping");
        result["keyHandle"] = handleKey({key->code, 0, true, key->x, key->y});
      }
    } else if (action == "testResize" && options_.headless) {
      const int width = request.at("width").get<int>(), height = request.at("height").get<int>();
      if (width < 1 || height < 1 || width > 4096 || height > 2160)
        throw std::runtime_error("Invalid diagnostic output size");
      std::string error;
      if (!resizeOutput(width, height, error))
        throw std::runtime_error(error);
      dirty_ = true;
      result = status();
    } else if (action == "testFrame" && options_.headless) {
      std::string error;
      if (!renderer_.hasRenderTarget() ||
          !renderer_.saveFrame(options_.stateDir / L"test-frame.png", error))
        throw std::runtime_error(error.empty() ? "No offscreen frame is available" : error);
      result = {{"framesRendered", framesRendered_}};
    } else if ((action == "perfReset" || action == "perfRead") && options_.headless && perf_) {
      if (action == "perfReset")
        perf_->reset();
      result = {{"status", status()}, {"performance", perf_->snapshot(renderer_.context())}};
    } else if (action == "refreshDevices") {
      const auto displays = monitors();
      chooseMonitor(true, displays);
      result = {{"monitors", listMonitors(displays)}, {"devices", listDevices(true)}};
    } else if (action == "listMonitors")
      result = listMonitors(monitors());
    else if (action == "listDevices")
      result = listDevices();
    else if (action == "listPalettes")
      result = listPalettes();
    else if (action == "listPacks") {
      result = listPacks(request);
    } else if (action == "getProtectedImportRevisions") {
      result = protectedImportRevisions(request.at("id").get<std::string>());
    } else if (action == "getPackSettings" || action == "setPackSettings" ||
               action == "resetPackSettings" || action == "previewPackSettings") {
      const auto id = request.at("id").get<std::string>();
      const auto &pack = requirePack(id);
      std::optional<std::uint64_t> expected;
      if (request.contains("expectedRevision")) {
        if (!request["expectedRevision"].is_number_integer() || request["expectedRevision"] < 1)
          throw std::runtime_error("Expected settings revision must be a positive integer");
        expected = request["expectedRevision"].get<std::uint64_t>();
      }
      auto candidate = action == "getPackSettings"
                           ? settings_.current(id)
                           : settings_.prepare(id,
                                               action == "resetPackSettings" ? json::object()
                                                                             : request.at("values"),
                                               expected, action == "resetPackSettings");
      const auto appearance = settings_.evaluate(candidate);
      validateEffectSettings(importUpdatePending(pack) ? *activeImportedPack_ : pack, appearance);
      if (id == packId_ && !graphicsSuspended_) {
        if (!effectIds_.empty() && appearance.playback)
          validateVariantSlots(*appearance.playback, effectIds_.size());
        std::string error;
        if (!validateUniforms(effectIds_, appearance, error))
          throw std::runtime_error(error);
      }
      const bool write = action == "setPackSettings" || action == "resetPackSettings";
      updateProfile(candidate, appearance, write ? std::optional<bool>(true) : std::nullopt);
      result = settings_.bundle(candidate);
    } else if (action == "reloadCalibration") {
      std::string error;
      if (!mapping_.tryReload(options_.root, error))
        throw std::runtime_error(error);
      if (keyboard_)
        keyboard_->setConfig(mapping_);
      stopEffects();
      result = status();
    } else if (action == "selectPack") {
      std::string error;
      if (!selectPack(request.at("id").get<std::string>(), error))
        throw std::runtime_error(error);
      preservedPackId_.clear();
      missingWallpaper_.reset();
      error_.clear();
      try {
        saveSettings();
      } catch (const std::exception &e) {
        // Runtime selection is already active; report a persistence warning
        // rather than falsely reporting that the actual asset switch itself
        // failed.
        error_ = std::string("Wallpaper applied, but settings were not saved: ") + e.what();
      }
      result = status();
    } else if (action == "setSettings") {
      const auto &changes = request.at("settings");
      if (!changes.is_object())
        throw std::runtime_error("Settings must be an object");
      const int fps = changes.value("fps", fps_);
      if (fps != 60)
        throw std::runtime_error("Frame rate must be 60");
      const auto group = changes.value("deviceGroup", mapping_.deviceGroup);
      if (group.size() > 1024)
        throw std::runtime_error("Device filter exceeds its limit");
      const bool requireConnectedKeyboard = request.value("requireConnectedKeyboard", false);
      const bool monitorChanged = changes.contains("monitorDevice");
      const auto monitor = changes.value("monitorDevice", storedMonitorDevice_);
      DisplayIdentity chosenIdentity;
      if (changes.contains("exclusiveDisplay") && !changes["exclusiveDisplay"].is_boolean())
        throw std::runtime_error("Exclusive display must be a boolean");
      const bool exclusiveDisplay = changes.value("exclusiveDisplay", exclusiveDisplay_);
      if (changes.contains("hideKeyboardTaskbar") && !changes["hideKeyboardTaskbar"].is_boolean())
        throw std::runtime_error("Hide keyboard taskbar must be a boolean");
      const bool hideKeyboardTaskbar = changes.value("hideKeyboardTaskbar", hideKeyboardTaskbar_);
      if (monitorChanged && !monitor.empty()) {
        const auto all = monitors();
        const auto selected = std::find_if(all.begin(), all.end(), [&](const Monitor &item) {
          return utf8(item.device) == monitor;
        });
        if (selected == all.end())
          throw std::runtime_error("Selected monitor is disconnected");
        if (selected->primary)
          throw std::runtime_error("The primary display cannot be used as "
                                   "keyboard wallpaper output");
        if (selected->identity.path.empty())
          throw std::runtime_error("无法确认所选显示器。请确认已使用扩展显示后刷新设备。");
        if (changes.contains("monitorIdentity") &&
            wide(changes.at("monitorIdentity").get<std::string>()) != selected->identity.path)
          throw std::runtime_error("显示器已变化，请刷新后重新选择。");
        chosenIdentity = selected->identity;
      }
      json legacy = json::object();
      for (auto setting = changes.begin(); setting != changes.end(); ++setting) {
        const auto &key = setting.key();
        if (key == "fps" || key == "monitorDevice" || key == "monitorIdentity" || key == "deviceGroup" ||
            key == "exclusiveDisplay" || key == "hideKeyboardTaskbar")
          continue;
        if (key == "effectBrightness" || key == "effectScale" || key == "effectIntensity" ||
            key == "effectSaturation" || key == "effectColor" || key == "effectPalette" ||
            key == "effectPaletteColors" || key == "effectHue")
          legacy[key] = setting.value();
        else if (key == "effectSpeed")
          legacy["effect.speed"] = setting.value();
        else if (key == "spriteDuration")
          legacy["sprite.duration"] = setting.value();
        else
          throw std::runtime_error("Unknown global setting: " + key);
      }
      if (requireConnectedKeyboard) {
        const auto devices = RawKeyboard::enumerate("");
        if (!devices)
          throw std::runtime_error("无法读取键盘设备，请重试刷新。");
        if (!changes.contains("deviceGroup") || !connectedKeyboardGroup(group, *devices))
          throw std::runtime_error("所选键盘已断开，请连接后刷新设备，或选择其他键盘。");
      }
      std::optional<PackSettingsCandidate> profile;
      PackAppearance appearance;
      if (!legacy.empty()) {
        if (packId_.empty())
          throw std::runtime_error("Select a wallpaper before changing its appearance");
        profile = settings_.prepare(packId_, settings_.legacyPatch(packId_, legacy));
        appearance = settings_.evaluate(*profile);
        if (!effectIds_.empty() && appearance.playback)
          validateVariantSlots(*appearance.playback, effectIds_.size());
        std::string error;
        if (!graphicsSuspended_ && !validateUniforms(effectIds_, appearance, error))
          throw std::runtime_error(error);
        updateProfile(*profile, appearance, true);
      }
      const bool rateChanged = fps_ != fps, deviceChanged = mapping_.deviceGroup != group,
                 guardChanged = exclusiveDisplay_ != exclusiveDisplay,
                 taskbarChanged = hideKeyboardTaskbar_ != hideKeyboardTaskbar;
      fps_ = fps;
      mapping_.deviceGroup = group;
      if (monitorChanged) {
        storedMonitorDevice_ = monitor;
        monitorIdentity_ = std::move(chosenIdentity);
      }
      exclusiveDisplay_ = exclusiveDisplay;
      hideKeyboardTaskbar_ = hideKeyboardTaskbar;
      if ((deviceChanged || requireConnectedKeyboard) && keyboard_)
        keyboard_->setConfig(mapping_);
      if (deviceChanged || monitorChanged)
        stopEffects();
      if (monitorChanged)
        chooseMonitor();
      else {
        if (guardChanged) {
          if (exclusiveDisplay_)
            resetPluginPointer();
          refreshDisplayGuard();
        }
        if (taskbarChanged)
          refreshKeyboardTaskbar();
      }
      if (rateChanged)
        pacer_.reset();
      error_.clear();
      try {
        saveSettings();
      } catch (const std::exception &exception) {
        error_ = std::string("Settings applied for this session: ") + exception.what();
      }
      result = status();
    } else if (action == "pause" || action == "resume") {
      const bool temporary = request.value("temporary", false);
      if (temporary && action == "pause") {
        const DWORD pid = request.at("ownerPid").get<DWORD>();
        if (temporaryPauseOwner_ && temporaryPauseOwnerPid_ != pid &&
            WaitForSingleObject(temporaryPauseOwner_, 0) != WAIT_OBJECT_0)
          throw std::runtime_error("Another manager is calibrating");
        const HANDLE owner = OpenProcess(SYNCHRONIZE, FALSE, pid);
        if (!owner)
          throw std::runtime_error("Cannot watch calibration manager process");
        if (temporaryPauseOwner_)
          CloseHandle(temporaryPauseOwner_);
        temporaryPauseOwner_ = owner;
        temporaryPauseOwnerPid_ = pid;
      } else if (temporary) {
        if (temporaryPauseOwner_ &&
            request.value("ownerPid", temporaryPauseOwnerPid_) == temporaryPauseOwnerPid_) {
          CloseHandle(temporaryPauseOwner_);
          temporaryPauseOwner_ = nullptr;
          temporaryPauseOwnerPid_ = 0;
        }
      } else {
        persistedPaused_ = action == "pause";
      }
      paused_ = persistedPaused_ || temporaryPauseOwner_ != nullptr;
      if (paused_)
        stopEffects();
      updateVisibility();
      lastFrame_ = std::chrono::steady_clock::now();
      if (!temporary)
        saveSettings();
      result = status();
    } else if (action == "hide" || action == "show") {
      visible_ = action == "show";
      if (!visible_)
        stopEffects();
      updateVisibility();
      saveSettings();
      result = status();
    } else if (action == "testPluginEvent" && options_.headless && scene_.plugin) {
      const auto type = request.at("type").get<unsigned>();
      if (type < A75_KEY_DOWN || type > A75_POINTER_UP)
        throw std::runtime_error("Unsupported diagnostic input event");
      if (!sendPluginEvent(type, request.value("x", 0.5f), request.value("y", 0.5f),
                           request.value("code", std::string()).c_str()))
        throw std::runtime_error(error_);
      result = status();
    } else if (action == "testTrigger" && options_.headless) {
      const auto code = request.at("code").get<std::string>();
      const auto fn = code == "Fn" ? mapping_.fnKey() : std::nullopt;
      const auto *key = fn ? &*fn : mapping_.findCode(code);
      if (!key || (effectId_ < 0 && scene_.empty()) || paused_ || !visible_)
        throw std::runtime_error("Diagnostic key cannot trigger the current state");
      if (request.contains("count") && !request["count"].is_number_integer())
        throw std::runtime_error("Diagnostic count must be an integer");
      const int count = request.value("count", 1);
      if (count < 1 || count > 128)
        throw std::runtime_error("Diagnostic count must be between 1 and 128");
      int handle = -1;
      int accepted = 0;
      for (int i = 0; i < count; ++i) {
        const auto triggered = trigger(key->x, key->y, key->code);
        if (triggered == SkippedTrigger)
          continue;
        if (triggered < 0)
          throw std::runtime_error("Diagnostic key could not trigger an effect");
        handle = triggered;
        ++accepted;
      }
      result = {{"handle", handle},
                {"x", key->x * renderer_.width()},
                {"y", key->y * renderer_.height()},
                {"effectVariantIndex", accepted > 0 ? lastTriggeredVariant_ : -1},
                {"acceptedTriggers", accepted}};
      if (scene_.fireworks && accepted > 0) {
        result["fireworkShape"] = scene_.fireworks->lastShape();
        result["fireworkPalette"] = scene_.fireworks->lastPalette();
      }
      if (scene_.glass)
        result["impactStrength"] = scene_.glass->lastStrength();
      if (effectId_ >= 0 && handle >= 0 &&
          std::any_of(activeAppearance_.dynamicInputs.begin(), activeAppearance_.dynamicInputs.end(),
                      [](const auto &input) { return input.has_value(); }))
        result["effectDynamicInputs"] = effects_.GetDynamicInputs(handle);
    } else if (action == "stop") {
      result = {{"stopping", true}};
      server_.stopAfterReply();
    } else
      throw std::runtime_error("Unknown player action");
    return {{"ok", true}, {"result", result}};
  } catch (const std::exception &e) {
    diagnostics_.write("ERROR", e.what());
    return {{"ok", false}, {"error", e.what()}};
  }
}
void Player::openManager(const wchar_t *argument) {
  if (options_.headless)
    return;
  const fs::path rustManager = options_.root / L"dist/Action75Manager/Action75Manager.exe";
  if (fs::is_regular_file(rustManager)) {
    std::wstring arguments = L"--root \"" + options_.root.wstring() + L"\"";
    if (argument)
      arguments += std::wstring(L" ") + argument;
    SHELLEXECUTEINFOW launch{};
    launch.cbSize = sizeof(launch);
    launch.fMask = SEE_MASK_FLAG_NO_UI;
    launch.lpFile = rustManager.c_str();
    launch.lpParameters = arguments.c_str();
    launch.lpDirectory = options_.root.c_str();
    launch.nShow = SW_SHOWNORMAL;
    if (!ShellExecuteExW(&launch))
      error_ = "Cannot open the Rust settings manager";
    return;
  }
  if (argument) {
    error_ = "The settings manager is not installed in this player directory";
    return;
  }
  std::wstring script = (options_.root / L"scripts/open-native-manager.ps1").wstring();
  if (!fs::is_regular_file(script)) {
    error_ = "The settings manager is not installed in this player directory";
    return;
  }
  std::wstring arguments = L"-NoProfile -ExecutionPolicy Bypass -File \"" + script + L"\"";
  SHELLEXECUTEINFOW info{};
  info.cbSize = sizeof(info);
  info.fMask = SEE_MASK_FLAG_NO_UI;
  info.lpFile = L"powershell.exe";
  info.lpParameters = arguments.c_str();
  info.lpDirectory = options_.root.c_str();
  info.nShow = SW_HIDE;
  ShellExecuteExW(&info);
}
void Player::resetPluginPointer(bool cancel) {
  if (!pluginPointerDown_)
    return;
  pluginPointerDown_ = false;
  TRACKMOUSEEVENT tracking{sizeof(tracking), TME_LEAVE | TME_CANCEL, window_, 0};
  TrackMouseEvent(&tracking);
  if (GetCapture() == window_)
    ReleaseCapture();
  if (cancel && scene_.plugin)
    sendPluginEvent(A75_INPUT_RESET);
}
bool Player::pluginPointerEnabled() const {
  return scene_.plugin && !desktopActive_ && !options_.headless && visible_ && outputReady() && !paused_;
}
void Player::pluginFailed() {
  const bool deviceLost = gpuDeviceLost();
  scene_.plugin.reset();
  resetPluginPointer();
  diagnostics_.write("WARN", error_);
  if (deviceLost) scheduleDeviceRecovery(error_);
  else pluginRecoveryPending_ = !options_.importTrial;
}
void Player::fallBackAfterWallpaperFailure() {
  if (preservedPackId_.empty())
    preservedPackId_ = packId_;
  pluginRecoveryPending_ = false;
  const auto fallback = std::exchange(pluginFallback_, {});
  std::string ignored;
  bool restored = false;
  try {
    restored = !fallback.empty() && fallback != packId_ && selectPack(fallback, ignored);
  } catch (const std::exception &failure) {
    diagnostics_.write("WARN", failure.what());
  }
  if (!restored)
    selectPack({}, ignored);
  pluginFallback_.clear();
}
void Player::retryMissingWallpaper(FramePacer::Time now) {
  auto &missing = *missingWallpaper_;
  std::error_code ec;
  if (!fs::is_regular_file(missing.module, ec)) {
    missing.delay = std::min(missing.delay * 2, std::chrono::seconds(8));
    missing.retryAt = now + missing.delay;
    return;
  }
  missingWallpaper_.reset();
  std::string error;
  try {
    refreshPacks();
    if (restoreSelection(preservedPackId_, error))
      preservedPackId_.clear();
  } catch (const std::exception &failure) {
    error = failure.what();
  }
  error_ = std::move(error);
  if (!error_.empty())
    diagnostics_.write("WARN", error_);
}
bool Player::sendPluginEvent(uint32_t type, float x, float y, const char *code, const char *values) {
  const A75Event event{sizeof(A75Event), type, renderer_.width(), renderer_.height(),
                       clockSeconds(FramePacer::Clock::now()), x, y, code, values};
  if (!scene_.plugin->event(event, error_)) {
    pluginFailed();
    return false;
  }
  const auto patch = scene_.plugin->takeSettingsPatch();
  if (!patch.empty() && type >= A75_KEY_DOWN && type <= A75_POINTER_UP && activeProfile_) {
    try {
      const auto valuesPatch = json::parse(patch);
      if (!valuesPatch.is_object())
        throw std::runtime_error("Plugin settings update must be an object");
      for (const auto &item : valuesPatch.items())
        if (!activeProfile_->values.contains(item.key()))
          throw std::runtime_error("Unknown plugin setting");
      auto updated = settings_.prepare(packId_, valuesPatch);
      updateProfile(updated, settings_.evaluate(updated), !options_.importTrial);
    } catch (const std::exception &exception) {
      error_ = exception.what();
      return false;
    }
  }
  // Continuous animations already have a paced deadline. Input wakes a
  // dormant scene without inserting an extra frame between scheduled frames.
  if (scene_.pluginDelay < 0)
    dirty_ = true;
  return true;
}
bool Player::canClickScene() const {
  return (scene_.water || scene_.fireworks || effectPlayback_) && !options_.headless && visible_ &&
         outputReady() && !paused_;
}
std::array<float, 2> Player::pointerPosition(POINT client) const {
  return {static_cast<float>(client.x) / static_cast<float>(renderer_.width()),
          static_cast<float>(client.y) / static_cast<float>(renderer_.height())};
}
void Player::registerTrayIcon() {
  if (tray_.cbSize && !Shell_NotifyIconW(NIM_ADD, &tray_))
    Shell_NotifyIconW(NIM_MODIFY, &tray_);
}

void Player::claimExitShortcut(HWND window) {
  if (!options_.headless && !exitShortcut_ && !quittingSession_)
    exitShortcut_ = RegisterHotKey(window, ExitHotkey, MOD_CONTROL | MOD_ALT | MOD_SHIFT | MOD_NOREPEAT, 'Q') != FALSE;
}
void Player::quitSession(HWND window) {
  if (quittingSession_)
    return;
  quittingSession_ = true;
  resetPluginPointer();
  displayGuard_.stop();
  keyboardTaskbar_.restore();
  // The event also reaches a panel still creating its first UI window.
  const auto name = L"Local\\" + options_.rootEndpoint + L"-ManagerQuit";
  HANDLE panel = OpenEventW(EVENT_MODIFY_STATE, FALSE, name.c_str());
  bool opening = false;
  if (panel) {
    opening = SetEvent(panel) != FALSE;
    CloseHandle(panel);
  }
  // A live panel drains pending settings, then sends the ordinary player stop.
  const bool notified = notifyManager(options_.rootEndpoint, QuitSessionMessage);
  if ((!notified && !opening) || !SetTimer(window, CloseTimer, 8000, nullptr))
    PostMessageW(window, WM_CLOSE, 0, 0);
}
LRESULT Player::message(HWND h, UINT m, WPARAM w, LPARAM l) {
  if (m && m == ClaimShortcutMessage) {
    claimExitShortcut(h);
    return 0;
  }
  if ((m && m == QuitSessionMessage) || (m == WM_HOTKEY && w == ExitHotkey)) {
    quitSession(h);
    return 0;
  }
  if (taskbarCreated_ && m == taskbarCreated_) {
    registerTrayIcon();
    refreshDesktopLayer();
    refreshKeyboardTaskbar();
    return 0;
  }
  if (keyboard_)
    keyboard_->process(m, w, l);
  switch (m) {
  case CommandMessage: {
    auto *call = reinterpret_cast<CommandCall *>(l);
    call->response = dispatch(*call->request);
    if (call->legacyCatalogWait && call->response.value("ok", false) &&
        call->response.at("result").contains("pendingCatalogRevision"))
      *call->legacyCatalogWait = catalogScan_;
    return 1;
  }
  case DesktopWallpaperLayer::Refresh:
    refreshDesktopLayer();
    return 0;
  case DesktopWallpaperLayer::Lost:
    if (desktopLayer_.get()==reinterpret_cast<HWND>(w)) {
      desktopLayer_.reset();
      refreshDesktopLayer();
    }
    return 0;
  case WM_DISPLAYCHANGE:
    chooseMonitor(true);
    return 0;
  case WM_DEVICECHANGE:
    if (!options_.headless && w == DBT_DEVNODES_CHANGED)
      SetTimer(h, MonitorTopologyTimer, 250, nullptr);
    break;
  case WM_SETTINGCHANGE:
    if (desktopActive_) PostMessageW(h,DesktopWallpaperLayer::Refresh,0,0);
    else if (keyboardZOrderTimer_)
      keepKeyboardAboveTaskbar(h);
    break;
  case WM_MOUSEACTIVATE:
    return MA_NOACTIVATE;
  case WM_NCHITTEST: {
    if (pluginPointerDown_)
      return HTCLIENT;
    if (pluginPointerEnabled()) {
      POINT client{GET_X_LPARAM(l), GET_Y_LPARAM(l)};
      if (ScreenToClient(h, &client)) {
        const auto pos = pointerPosition(client);
        bool hit = false;
        if (scene_.plugin->hitTest(pos[0], pos[1], hit, error_) && hit)
          return HTCLIENT;
      }
    }
    if (canClickScene())
      return HTCLIENT;
    return HTTRANSPARENT;
  }
  case WM_LBUTTONDOWN:
    if (pluginPointerEnabled()) {
      if (pluginPointerDown_)
        return 0;
      const auto pos = pointerPosition({GET_X_LPARAM(l), GET_Y_LPARAM(l)});
      pluginPointerDown_ = true;
      if (sendPluginEvent(A75_POINTER_DOWN, pos[0], pos[1])) {
        SetCapture(h);
        TRACKMOUSEEVENT tracking{sizeof(tracking), TME_LEAVE, h, 0};
        if (GetCapture() == h && TrackMouseEvent(&tracking))
          return 0;
      }
      resetPluginPointer();
      return 0;
    }
    if (canClickScene()) {
      const auto point = pointerPosition({GET_X_LPARAM(l), GET_Y_LPARAM(l)});
      trigger(point[0], point[1]);
      return 0;
    }
    break;
  case WM_MOUSEMOVE:
    if (pluginPointerEnabled()) {
      const auto pos = pointerPosition({GET_X_LPARAM(l), GET_Y_LPARAM(l)});
      if (pluginPointerDown_ && (!(w & MK_LBUTTON) || pos[0] < 0 || pos[0] >= 1 ||
                                 pos[1] < 0 || pos[1] >= 1)) {
        resetPluginPointer();
        return 0;
      }
      sendPluginEvent(A75_POINTER_MOVE, pos[0], pos[1]);
      return 0;
    }
    break;
  case WM_LBUTTONUP:
    if (pluginPointerDown_) {
      if (pluginPointerEnabled()) {
        const auto pos = pointerPosition({GET_X_LPARAM(l), GET_Y_LPARAM(l)});
        resetPluginPointer(false);
        sendPluginEvent(A75_POINTER_UP, pos[0], pos[1]);
      } else {
        resetPluginPointer();
      }
      return 0;
    }
    break;
  case WM_MOUSELEAVE:
  case WM_CAPTURECHANGED:
    resetPluginPointer();
    return 0;
  case WM_CANCELMODE:
    resetPluginPointer();
    break;
  case WM_ERASEBKGND:
    return 1;
  case WM_PAINT: {
    PAINTSTRUCT paint;
    BeginPaint(h, &paint);
    EndPaint(h, &paint);
    dirty_ = true;
    return 0;
  }
  case WM_TIMER:
    if (w == CloseTimer) {
      KillTimer(h, CloseTimer);
      DestroyWindow(h);
    }
    if (w == KeyboardZOrderTimer)
      keepKeyboardAboveTaskbar(h);
    if (w == MonitorTopologyTimer) {
      KillTimer(h, MonitorTopologyTimer);
      chooseMonitor(true);
    }
    return 0;
  case WM_CLOSE:
    DestroyWindow(h);
    return 0;
  case WM_DESTROY:
    desktopLayer_.reset();
    if (exitShortcut_) {
      UnregisterHotKey(h, ExitHotkey);
      exitShortcut_ = false;
      if (!quittingSession_) notifyManager(options_.rootEndpoint, ClaimShortcutMessage);
    }
    RemovePropW(h, (options_.rootEndpoint + L"-PlayerWindow").c_str());
    resetPluginPointer();
    displayGuard_.stop();
    keyboardTaskbar_.restore();
    server_.requestStop();
    PostQuitMessage(0);
    return 0;
  case WM_NCDESTROY:
    RemovePropW(h, L"NonRudeHWND");
    break;
  case WM_MEASUREITEM:
    if (activeTrayMenu_ && l && activeTrayMenu_->measure(*reinterpret_cast<MEASUREITEMSTRUCT *>(l)))
      return TRUE;
    break;
  case WM_DRAWITEM:
    if (activeTrayMenu_ && l && activeTrayMenu_->draw(*reinterpret_cast<const DRAWITEMSTRUCT *>(l)))
      return TRUE;
    break;
  case TrayMessage:
    if (l == WM_LBUTTONDBLCLK) {
      openManager();
      return 0;
    }
    if (l == WM_RBUTTONUP) {
      if (activeTrayMenu_) return 0;
      POINT point{};
      if (!GetCursorPos(&point)) return 0;
      std::optional<bool> graphite;
      try {
        const auto theme = readJson(options_.root / L"data/app-settings.json").value("theme", std::string{});
        if (theme == "graphite" || theme == "paper") graphite = theme == "graphite";
      } catch (const std::exception &) {
        // An unconfigured manager leaves the tray on the system theme.
      }
      TrayMenu appearance(point, graphite);
      const HMENU menu = appearance.root();
      if (!menu) return 0;
      appearance.append(menu, 1, L"打开主界面");
      const bool resumeInteraction = temporaryPauseOwner_ ? persistedPaused_ : paused_;
      appearance.append(menu, 2, resumeInteraction ? L"恢复互动" : L"暂停互动");
      appearance.append(menu, 3, visible_ ? L"隐藏副屏" : L"显示副屏");
      std::vector<std::string> packChoices;
      const HMENU themes = appearance.createPopup();
      if (themes) {
        for (const auto &p : packs_) {
          if (!p.supported || packChoices.size() >= 20)
            continue;
          const UINT id = static_cast<UINT>(100 + packChoices.size());
          if (appearance.append(themes, id, wide(p.name), p.id == packId_))
            packChoices.push_back(p.id);
        }
        if (packChoices.empty() ||
            !appearance.append(menu, 0, L"互动壁纸", false, themes))
          DestroyMenu(themes);
      }
      HKEY runKey = nullptr;
      bool autostart = false;
      if (RegOpenKeyExW(HKEY_CURRENT_USER, L"Software\\Microsoft\\Windows\\CurrentVersion\\Run", 0,
                        KEY_QUERY_VALUE, &runKey) == ERROR_SUCCESS) {
        autostart = RegQueryValueExW(runKey, L"ACTION75", nullptr, nullptr, nullptr, nullptr) ==
                    ERROR_SUCCESS;
        RegCloseKey(runKey);
      }
      appearance.append(menu, 24, L"开机运行", autostart);
      AppendMenuW(menu, MF_SEPARATOR, 0, nullptr);
      appearance.append(menu, 4, L"退出");
      activeTrayMenu_ = &appearance;
      const auto customChoice = appearance.show(h, point);
      UINT choice = 0;
      if (customChoice) {
        choice = *customChoice;
        diagnostics_.write("INFO", "Action75 tray popup closed");
      } else {
        SetForegroundWindow(h);
        if (const auto popup = appearance.popupAnchor()) {
          choice = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_BOTTOMALIGN | TPM_RIGHTALIGN,
                                  popup->x, popup->y, 0, h, nullptr);
        } else {
          choice = TrackPopupMenu(menu, TPM_RETURNCMD, point.x, point.y, 0, h, nullptr);
        }
      }
      activeTrayMenu_ = nullptr;
      if (choice == 1)
        openManager();
      else if (choice == 2)
        dispatch({{"action", resumeInteraction ? "resume" : "pause"}});
      else if (choice == 3)
        dispatch({{"action", visible_ ? "hide" : "show"}});
      else if (choice == 4)
        quitSession(h);
      else if (choice == 24)
        openManager(L"--toggle-startup");
      else if (choice >= 100 && choice - 100 < packChoices.size()) {
        const auto reply = dispatch({{"action", "selectPack"}, {"id", packChoices[choice - 100]}});
        if (!reply.value("ok", false))
          error_ = reply.value("error", std::string("Wallpaper could not be selected"));
      }
      PostMessageW(h, WM_NULL, 0, 0);
      return 0;
    }
    break;
  }
  return DefWindowProcW(h, m, w, l);
}
bool Player::hasMotion() const {
  return visible_ && (outputReady() || offscreenOnly()) &&
         (renderer_.presentPending() || renderer_.isAnimated() ||
          (!paused_ && ((scene_.plugin && scene_.pluginDelay >= 0) || (scene_.water && scene_.water->hasMotion()) ||
                        (scene_.fireworks && scene_.fireworks->count()) ||
                        (scene_.glass && scene_.glass->count()) ||
                        (scene_.procedural && scene_.procedural->count()) || scene_.field ||
                        (scene_.keycaps && (scene_.keycaps->count() || scene_.keycaps->animated())) ||
                        (scene_.sprite && scene_.sprite->count()) || effects_.HasVisibleEffects())));
}

void Player::scheduleDeviceRecovery(const std::string &error) {
  if (deviceRetryAt_ || monitorSuspendAt_ || (graphicsSuspended_ && !hasMonitor_))
    return;
  deviceRetryAt_ = FramePacer::Clock::now();
  error_ = error.empty() ? "Graphics device was lost" : error;
  diagnostics_.write("WARN", error_);
}

void Player::releaseGraphics() {
  dx12PresentCounters_.discardPending();
  resetPluginPointer(false);
  scene_ = {};
  pluginRecoveryPending_ = false;
  effects_.Shutdown();
  effekseerReady_ = false;
  effectId_ = -1;
  effectIds_.clear();
  effectPlayback_.reset();
  perf_.reset();
  FieldEffect::releaseDevicePrograms(renderer_.device());
  renderer_.releaseResources();
  desktopLayer_.reset();
  desktopActive_=false;
}

void Player::suspendGraphics() {
  monitorSuspendAt_.reset();
  deviceRetryAt_.reset();
  graphicsSuspended_ = true;
  releaseGraphics();
  pacer_.reset();
  diagnostics_.write("INFO", "Graphics released while the keyboard display is disconnected");
}

void Player::recoverGraphics() {
  std::string error;
  bool restored = false;
  try {
    restored = restoreDevice(error);
  } catch (const std::exception &failure) {
    error = failure.what();
  }
  if (restored) {
    deviceRetryAt_.reset();
    ignoreSwapWait_ = false;
    swapWait_.ready();
    diagnostics_.write(error.empty() ? "INFO" : "WARN",
                       error.empty() ? "Graphics device restored" : error);
  } else {
    if (graphicsSuspended_)
      releaseGraphics();
    deviceRetryAt_ = FramePacer::Clock::now() + std::chrono::seconds(1);
    if (error != error_)
      diagnostics_.write("WARN", error);
  }
  error_ = std::move(error);
}

bool Player::restoreDevice(std::string &error) {
  if (packId_.empty())
    return true;
  const bool reconnecting = graphicsSuspended_;
  unsigned width = renderer_.width(), height = renderer_.height();
  RECT client{};
  if (usesOutputWindow() && GetClientRect(window_, &client) &&
      client.right > client.left && client.bottom > client.top) {
    width = static_cast<unsigned>(client.right - client.left);
    height = static_cast<unsigned>(client.bottom - client.top);
  }
  if (!width || !height) {
    width = 1920;
    height = 720;
  }

  releaseGraphics();

  std::error_code fileError;
  const bool missingModule = !activeNativeModule_.empty() &&
                             !fs::is_regular_file(activeNativeModule_, fileError);
  auto *selected = selectedPack();
  if (!selected && !activeNativeModule_.empty() && !missingModule) {
    refreshPacks();
    selected = selectedPack();
  }
  const bool use12 = selected ? selected->usesD3D12 : activeNativeUsesD3D12_;
  if (!initializeOutput(use12, width, height, false, error))
    return false;
  if (!reconnecting)
    chooseMonitor();
  if (!renderer_.hasRenderTarget()) {
    error = error_.empty() ? "Graphics output could not be restored" : error_;
    return false;
  }

  if (!packId_.empty()) {
    const Pack *active = selectedPack();
    if (missingModule)
      error = "The active wallpaper DLL is unavailable";
    else if (!active)
      error = "The active wallpaper is no longer available";
    if (!active || missingModule || !activatePack(active, error, true)) {
      if (gpuDeviceLost())
        return false;
      if (missingModule && (preservedPackId_.empty() || preservedPackId_ == packId_))
        missingWallpaper_ = MissingWallpaper{activeNativeModule_,
                                            FramePacer::Clock::now() + std::chrono::seconds(1)};
      // Recover a previous wallpaper or release graphics while retrying a missing DLL.
      graphicsSuspended_ = false;
      deviceRetryAt_.reset();
      fallBackAfterWallpaperFailure();
      if (!packId_.empty() && (!renderer_.hasRenderTarget() || gpuDeviceLost()))
        return false;
    }
  }
  lastFrame_ = FramePacer::Clock::now();
  pacer_.reset();
  if (reconnecting) {
    graphicsSuspended_ = false;
    updateVisibility();
  }
  return true;
}

void Player::render() {
  const bool phaseReset = dirty_;
  const auto *output12 = options_.dx12PresentStats ? renderer_.dx12() : nullptr;
  const bool countDraw = output12 && output12->hasWindowPresentation() && scene_.plugin;
  const bool idleDraw = countDraw && !phaseReset && scene_.pluginDelay < 0;
  if (countDraw)
    ++dx12PresentCounters_.drawCalls;
  auto now = std::chrono::steady_clock::now();
  float elapsed = std::chrono::duration<float>(now - lastFrame_).count();
  lastFrame_ = now;
  auto begin = now;
  if (perf_)
    perf_->begin(renderer_.context());
  bool drew = scene_.draw(renderer_, clockSeconds(now), paused_, error_);
  if (scene_.plugin && !drew)
    pluginFailed();
  const auto backgroundError = renderer_.backgroundError();
  if (drew && !backgroundError.empty())
    error_ = backgroundError;
  if (drew && !paused_ && effects_.HasActiveEffects()) {
    effects_.SetBackground(renderer_.background());
    effects_.UpdateForFrame(std::min(elapsed, 0.1f) * effectSpeed_);
    drew = effects_.Draw();
    if (!drew)
      error_ = effects_.LastError();
  }
  const bool submitted = drew && renderer_.present(error_);
  if (countDraw && drew)
    dx12PresentCounters_.recordPresent(renderer_.lastPresentResult(), true, idleDraw);
  if (!submitted && gpuDeviceLost()) {
    scheduleDeviceRecovery(error_);
  } else if (drew && !submitted) {
    paused_ = true;
    updateVisibility();
  }
  frameMs_ =
      std::chrono::duration<float, std::milli>(std::chrono::steady_clock::now() - begin).count();
  if (perf_)
    perf_->end(renderer_.context(), frameMs_);
  dirty_ = false;
  if (submitted)
    ++framesRendered_;
  pacer_.rendered(now, renderFps(), hasMotion(), phaseReset);
}

static void checkImportImages(const fs::path &directory, const fs::path &coverToDecode,
                              const fs::path &mediaImage) {
  Microsoft::WRL::ComPtr<IWICImagingFactory> factory;
  if (FAILED(CoCreateInstance(CLSID_WICImagingFactory, nullptr, CLSCTX_INPROC_SERVER,
                              IID_PPV_ARGS(factory.GetAddressOf()))))
    throw std::runtime_error("无法初始化图片检查");
  std::uint64_t pixels = 0;
  for (const auto &entry : fs::recursive_directory_iterator(directory)) {
    if (!entry.is_regular_file())
      continue;
    auto extension = entry.path().extension().wstring();
    std::transform(extension.begin(), extension.end(), extension.begin(),
                   [](wchar_t c) { return std::towlower(c); });
    const bool source = !mediaImage.empty() && fs::equivalent(entry.path(), mediaImage);
    const GUID *expected=nullptr;
    if (extension==L".png") expected=&GUID_ContainerFormatPng;
    else if (extension==L".jpg" || extension==L".jpeg") expected=&GUID_ContainerFormatJpeg;
    else if (source && extension==L".bmp") expected=&GUID_ContainerFormatBmp;
    else if (source && (extension==L".tif" || extension==L".tiff")) expected=&GUID_ContainerFormatTiff;
    else if (source && extension==L".webp") expected=&GUID_ContainerFormatWebp;
    else continue;
    Microsoft::WRL::ComPtr<IWICBitmapDecoder> decoder;
    Microsoft::WRL::ComPtr<IWICBitmapFrameDecode> frame;
    UINT width = 0, height = 0;
    const auto name = entry.path().lexically_relative(directory).u8string();
    if (FAILED(factory->CreateDecoderFromFilename(entry.path().c_str(), nullptr, GENERIC_READ,
                                                  WICDecodeMetadataCacheOnLoad, &decoder)) ||
        FAILED(decoder->GetFrame(0, &frame)) || FAILED(frame->GetSize(&width, &height)))
      throw std::runtime_error("无法解码图片：" + name);
    GUID format{};
    if (FAILED(decoder->GetContainerFormat(&format)) || !IsEqualGUID(format,*expected))
      throw std::runtime_error("图片内容与扩展名不符：" + name);
    const auto area=std::uint64_t(width)*height;
    if (!source) pixels+=area;
    const unsigned sideLimit=source ? 16384 : 4096;
    if (!width || !height || width>sideLimit || height>sideLimit ||
        (source ? area>33554432u : pixels>16*1024*1024u))
      throw std::runtime_error("图片尺寸或像素总量超出导入上限：" + name);
    if (!coverToDecode.empty() && fs::equivalent(entry.path(), coverToDecode)) {
      // Decode the cover without allocating a graphics device inside the trial job.
      Microsoft::WRL::ComPtr<IWICFormatConverter> rgba;
      if (FAILED(factory->CreateFormatConverter(&rgba)) ||
          FAILED(rgba->Initialize(frame.Get(), GUID_WICPixelFormat32bppRGBA,
                                  WICBitmapDitherTypeNone, nullptr, 0, WICBitmapPaletteTypeCustom)))
        throw std::runtime_error("Cannot decode wallpaper cover");
      std::vector<BYTE> strip(width * 4 * std::min(height, 32u));
      for (UINT y = 0; y < height; y += 32) {
        WICRect bounds{0, INT(y), INT(width), INT(std::min(32u, height - y))};
        if (FAILED(rgba->CopyPixels(&bounds, width * 4, UINT(strip.size()), strip.data())))
          throw std::runtime_error("Cannot decode wallpaper cover pixels");
      }
    }
  }
}

json Player::trialWallpaper() {
  refreshPacks();
  if (packs_.size() != 1 || !packs_.front().supported)
    throw std::runtime_error("清单的后端参数不符合要求，请检查图集格数、播放时长或效果大小");
  const auto &pack = packs_.front();
  if (!pack.importTrialAllowed)
    throw std::runtime_error("该壁纸类型不支持导入试播");
  const auto manifest = readJson(pack.directory / L"manifest.json");
  const auto preview = manifest.contains("preview")
                           ? packAsset(pack.directory, manifest.at("preview").get<std::string>())
                           : pack.directory / L"preview.png";
  const bool hasCover = fs::is_regular_file(preview);
  checkImportImages(pack.directory, pack.usesD3D12 && hasCover ? preview : fs::path{},
                    pack.kind == "image" ? pack.background : fs::path{});
  gpuPreference_ = options_.integratedTrial ? GpuPreference::Integrated : GpuPreference::Discrete;
  if (!initializeOutput(pack.usesD3D12, 1920, 720, pack.kind == "video", error_))
    throw std::runtime_error(error_);
  if (!pack.usesD3D12 && hasCover && !renderer_.loadBackground(preview, error_))
    throw std::runtime_error(error_);
  if (!selectPack(pack.id, error_))
    throw std::runtime_error(error_);
  const auto handle = trigger(0.5f, 0.5f, scene_.plugin ? "Space" : "");
  if (effectPlayback_ && handle < 0)
    throw std::runtime_error("Variant trial did not trigger a selected Effekseer effect");
  lastFrame_ = std::chrono::steady_clock::now();
  const auto deadline=lastFrame_+std::chrono::seconds(8);
  const auto trialFrame=options_.root / L"trial-frame.png";
  const bool video = pack.kind == "video";
  const int trialFrames = pack.kind == "image" || video ? 1 : 6;
  for (int frame = 0; frame < trialFrames || (video && !renderer_.background()); ++frame) {
    if (video && FramePacer::Clock::now() >= deadline)
      throw std::runtime_error("无法解码视频首帧，请检查视频编码或安装对应的 Windows 解码器。");
    Sleep(16);
    if (frame == 3 && scene_.plugin && !sendPluginEvent(A75_KEY_UP, 0.5f, 0.5f, "Space"))
      throw std::runtime_error(error_);
    render();
    if (!error_.empty())
      throw std::runtime_error(error_);
  }
  // Both output paths wait for GPU completion before encoding the trial PNG.
  if (!renderer_.saveFrame(trialFrame, error_))
    throw std::runtime_error(error_);
  json report = {{"ok", true}, {"framesRendered", framesRendered_}, {"triggered", handle >= 0},
                 {"playerPid", GetCurrentProcessId()}, {"renderer", renderer_.dx12() ? "D3D12" : "D3D11"},
                 {"gpuAdapter", utf8(renderer_.adapterName())},
                 {"hostD3D11Device", renderer_.device() != nullptr},
                 {"hostD3D12Device", renderer_.dx12() && renderer_.dx12()->device()},
                 {"nativeWorkerPid", scene_.plugin ? scene_.plugin->workerPid() : 0},
                 {"nativeWorkerAbi", scene_.plugin ? scene_.plugin->renderAbi() : 0}};
  if (scene_.fireworks && handle >= 0) {
    report["fireworkShape"] = scene_.fireworks->lastShape();
    report["fireworkPalette"] = scene_.fireworks->lastPalette();
  }
  if (effectPlayback_)
    report["effectVariantIndex"] = lastTriggeredVariant_;
  return report;
}

int Player::run() {
  diagnostics_.initialize(options_.root);
  refreshPacks();
  const auto file = options_.stateDir / L"native-player.json";
  if (fs::exists(file)) {
    try {
      const auto stored = readJson(file);
      fps_ = 60;
      storedMonitorDevice_ = stored.value("monitorDevice", std::string());
      const auto targetPath = stored.value("monitorTargetPath", std::string());
      const auto targetSerial = stored.value("monitorTargetSerial", std::string());
      if (targetPath.size() <= 1024 && targetSerial.size() <= 128)
        monitorIdentity_ = {wide(targetPath), wide(targetSerial)};
      mapping_.deviceGroup = stored.value("deviceGroup", mapping_.deviceGroup);
      if (mapping_.deviceGroup.size() > 1024)
        mapping_.deviceGroup = "VID_1CA6&PID_1A0B";
      if (storedMonitorDevice_.size() > 1024)
        storedMonitorDevice_.clear();
      packId_ = stored.value("packId", std::string());
      paused_ = stored.value("paused", false);
      visible_ = stored.value("visible", true);
      exclusiveDisplay_ = stored.value("exclusiveDisplay", false);
      hideKeyboardTaskbar_ = stored.value("hideKeyboardTaskbar", false);
      legacyPackId_ = packId_;
      for (const char *field :
           {"effectBrightness", "effectScale", "effectIntensity", "effectSaturation", "effectColor",
            "effectPalette", "effectPaletteColors", "effectHue"})
        if (stored.contains(field))
          legacyAppearance_[field] = stored[field];
    } catch (const std::exception &e) {
      fps_ = 60;
      storedMonitorDevice_.clear();
      monitorIdentity_ = {};
      packId_.clear();
      legacyPackId_.clear();
      legacyAppearance_ = json::object();
      mapping_.deviceGroup = "VID_1CA6&PID_1A0B";
      paused_ = false;
      visible_ = true;
      exclusiveDisplay_ = false;
      hideKeyboardTaskbar_ = false;
      error_ = std::string("Settings could not be loaded; defaults are active: ") + e.what();
    }
  } else if (!options_.headless) {
    const auto first =
        std::find_if(packs_.begin(), packs_.end(), [](const Pack &pack) { return pack.supported; });
    if (first != packs_.end())
      packId_ = first->id;
  }
  persistedPaused_ = paused_;
  WNDCLASSEXW wc{};
  wc.cbSize = sizeof(wc);
  wc.hInstance = GetModuleHandleW(nullptr);
  wc.lpfnWndProc = procedure;
  wc.lpszClassName = L"Action75NativeWindow";
  wc.hCursor = LoadCursor(nullptr, IDC_ARROW);
  RegisterClassExW(&wc);
  window_ = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_TOPMOST,
                            wc.lpszClassName, L"Action75 Wallpaper · 键盘屏", WS_POPUP, 0, 0,
                            1920, 720,
                            nullptr, nullptr, wc.hInstance, this);
  if (!window_)
    throw std::runtime_error("Cannot create native output window");
  // The keyboard output fills its monitor without entering Explorer's full-screen mode.
  if (!options_.headless)
    SetPropW(window_, L"NonRudeHWND", reinterpret_cast<HANDLE>(TRUE));
  gpuPreference_ = requestedGpu(options_.root);
  const std::string requested = std::move(packId_);
  packId_.clear();
  chooseMonitor();
  if (!requested.empty()) {
    if (restoreSelection(requested, error_)) {
      try {
        saveSettings();
      } catch (const std::exception &exception) {
        error_ = exception.what();
      }
    } else {
      preservedPackId_ = requested;
    }
  }
  if (!options_.headless) {
    keyboard_ = std::make_unique<RawKeyboard>(mapping_, [this](const KeyEvent &event) { handleKey(event); });
    if (!keyboard_->attach(window_))
      throw std::runtime_error(keyboard_->lastError());
    icon_ = static_cast<HICON>(LoadImageW(nullptr, (options_.root / L"assets/app.ico").c_str(),
                                          IMAGE_ICON, 0, 0, LR_LOADFROMFILE | LR_DEFAULTSIZE));
    tray_.cbSize = sizeof(tray_);
    tray_.hWnd = window_;
    tray_.uID = 1;
    tray_.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    tray_.uCallbackMessage = TrayMessage;
    tray_.hIcon = icon_ ? icon_ : LoadIcon(nullptr, IDI_APPLICATION);
    wcscpy_s(tray_.szTip, L"Action75 Wallpaper");
    taskbarCreated_ = RegisterWindowMessageW(L"TaskbarCreated");
    if (taskbarCreated_)
      ChangeWindowMessageFilterEx(window_, taskbarCreated_, MSGFLT_ALLOW, nullptr);
    registerTrayIcon();
  }
  SetPropW(window_, (options_.rootEndpoint + L"-PlayerWindow").c_str(), reinterpret_cast<HANDLE>(1));
  claimExitShortcut(window_);
  if (!server_.start(options_.pipe, [this](const json &request) {
        const auto onPlayerThread = [this](const json &query, CatalogScan *wait = nullptr) {
          CommandCall call{&query, {}, wait};
          if (!IsWindow(window_) ||
              !SendMessageW(window_, CommandMessage, 0, reinterpret_cast<LPARAM>(&call)))
            return json{{"ok", false}, {"error", "Player is stopping"}};
          return std::move(call.response);
        };
        const bool legacyList = request.value("action", std::string()) == "listPacks" &&
                                !request.value("asyncRefresh", false);
        CatalogScan pending;
        auto response = onPlayerThread(request, legacyList ? &pending : nullptr);
        if (!pending.valid())
          return response;
        // Preserve the original replies for older clients. Their blocking wait
        // must stay on the IPC thread while raw input and rendering continue.
        if (pending.wait_for(std::chrono::seconds(5)) != std::future_status::ready)
          return json{{"ok", false}, {"error", "Wallpaper catalog refresh timed out"}};
        auto poll = request;
        poll["catalogRefreshRevision"] = response.at("result").at("pendingCatalogRevision");
        return onPlayerThread(poll);
      }))
    throw std::runtime_error("Cannot start local control pipe");
  struct OneShotTimer {
    HANDLE handle = nullptr;
    bool armed = false;
    FramePacer::Time deadline{};
    ~OneShotTimer() {
      if (handle)
        CloseHandle(handle);
    }
    bool arm(FramePacer::Time now, FramePacer::Duration left) noexcept {
      if (!handle)
        return false;
      if (armed && now + left == deadline)
        return true;
      LARGE_INTEGER due{};
      due.QuadPart = FramePacer::relativeHundredNanoseconds(left);
      armed = SetWaitableTimer(handle, &due, 0, nullptr, nullptr, FALSE) != FALSE;
      if (armed)
        deadline = now + left;
      return armed;
    }
    void cancel() noexcept {
      if (armed) {
        CancelWaitableTimer(handle);
        armed = false;
      }
    }
  };
  // CREATE_WAITABLE_TIMER_HIGH_RESOLUTION (Windows 10 1803+).
  // Use a literal for compilation with older Windows SDK headers.
  constexpr DWORD HighResolutionTimerFlag = 0x00000002u;
  OneShotTimer frameTimer;
  frameTimer.handle = CreateWaitableTimerExW(nullptr, nullptr, HighResolutionTimerFlag,
                                             TIMER_MODIFY_STATE | SYNCHRONIZE);
  highResolutionWait_ = frameTimer.handle != nullptr;
  MSG msg{};
  bool running = true;
  while (running) {
    // Even a continuous WM_INPUT/IPC load cannot starve a due frame
    // indefinitely.
    const auto dispatchLimit = FramePacer::Clock::now() + std::chrono::milliseconds(1);
    for (unsigned count = 0; count < 64 && PeekMessageW(&msg, nullptr, 0, 0, PM_REMOVE); ++count) {
      if (msg.message == WM_QUIT) {
        running = false;
        break;
      }
      TranslateMessage(&msg);
      DispatchMessageW(&msg);
      if (FramePacer::Clock::now() >= dispatchLimit)
        break;
    }
    if (!running)
      break;
    if (WaitForSingleObject(server_.stoppingHandle(), 0) == WAIT_OBJECT_0) {
      DestroyWindow(window_);
      break;
    }
    if (scene_.plugin && !scene_.plugin->active()) {
      error_ = "Wallpaper worker exited";
      pluginFailed();
    }
    if (pluginRecoveryPending_ && !monitorSuspendAt_) {
      const auto failure = packId_ + ": " + error_;
      fallBackAfterWallpaperFailure();
      error_ = failure;
      continue;
    }
    if (temporaryPauseOwner_ && WaitForSingleObject(temporaryPauseOwner_, 0) == WAIT_OBJECT_0) {
      CloseHandle(temporaryPauseOwner_);
      temporaryPauseOwner_ = nullptr;
      temporaryPauseOwnerPid_ = 0;
      paused_ = persistedPaused_;
      updateVisibility();
      lastFrame_ = FramePacer::Clock::now();
    }
    const auto now = FramePacer::Clock::now();
    if (monitorSuspendAt_ && now >= *monitorSuspendAt_) {
      // A display notification can still be in flight when the deadline expires.
      chooseMonitor(true);
      if (monitorSuspendAt_)
        suspendGraphics();
      continue;
    }
    const auto removed = renderer_.deviceRemovedEvent();
    if (!deviceRetryAt_ && !monitorSuspendAt_ &&
        ((removed && WaitForSingleObject(removed, 0) == WAIT_OBJECT_0) ||
         (!renderer_.hasRenderTarget() && gpuDeviceLost())))
      scheduleDeviceRecovery("Graphics device was lost");
    if (deviceRetryAt_ && now >= *deviceRetryAt_) {
      recoverGraphics();
      continue;
    }
    if (!deviceRetryAt_ && outputRetryAt_ && now >= *outputRetryAt_) {
      chooseMonitor(true);
      continue;
    }
    const bool canRetryWallpaper = missingWallpaper_ && !deviceRetryAt_ &&
                                   hasMonitor_ && !graphicsSuspended_ && !outputRetryAt_;
    if (canRetryWallpaper && now >= missingWallpaper_->retryAt) {
      retryMissingWallpaper(now);
      continue;
    }
    pacer_.setRate(renderFps());
    const auto requestedInterval = scene_.plugin && !pluginPaused_ && scene_.pluginDelay > 0 &&
                                   !renderer_.presentPending()
        ? std::chrono::milliseconds(scene_.pluginDelay) : std::chrono::milliseconds(0);
    auto plan = deviceRetryAt_
        ? FramePacer::Plan{false, true, *deviceRetryAt_ - now}
        : pacer_.plan(now, renderer_.hasRenderTarget() &&
                               (offscreenOnly() || (visible_ && outputReady())),
                      dirty_, hasMotion(), requestedInterval);
    const auto wallpaperRetryAt = canRetryWallpaper
        ? std::optional{missingWallpaper_->retryAt} : std::nullopt;
    for (const auto &retry : {monitorSuspendAt_, deviceRetryAt_ ? std::nullopt : outputRetryAt_,
                             wallpaperRetryAt}) {
      if (retry && !plan.due) {
        const auto remaining = *retry - now;
        if (!plan.timed || remaining < plan.remaining)
          plan = {false, true, remaining};
      }
    }
    if (plan.due) {
      if (!ignoreSwapWait_ && renderer_.frameReady()) {
        const auto immediate = WaitForSingleObject(renderer_.frameReady(), 0);
        if (immediate == WAIT_TIMEOUT) {
          const auto timeout = swapWait_.waitMilliseconds(FramePacer::Clock::now());
          if (timeout) {
            HANDLE waits[4] = {renderer_.frameReady(), server_.stoppingHandle()};
            DWORD count = 2;
            if (temporaryPauseOwner_)
              waits[count++] = temporaryPauseOwner_;
            if (removed)
              waits[count++] = removed;
            const DWORD ready = MsgWaitForMultipleObjectsEx(
                count, waits, timeout, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
            // The event may be auto-reset. Do not poll after consuming it.
            if (ready == WAIT_OBJECT_0) {
              swapWait_.ready();
              render();
            } else if (ready == WAIT_TIMEOUT) {
              if (!swapWait_.waitMilliseconds(FramePacer::Clock::now()))
                render();
            } else if (ready == WAIT_FAILED) {
              ignoreSwapWait_ = true;
              error_ = "DXGI frame-latency wait failed; continuing without the "
                       "wait object.";
            }
            // Input/IPC wakes retain the original frame deadline. An absent
            // latency event cannot indefinitely prevent rendering.
            continue;
          }
        } else if (immediate == WAIT_OBJECT_0) {
          swapWait_.ready();
        } else if (immediate == WAIT_FAILED) {
          ignoreSwapWait_ = true;
          error_ = "DXGI frame-latency wait failed; continuing without the "
                   "wait object.";
        }
      }
      render();
      continue;
    }
    swapWait_.cancelPending();
    DWORD timeout = INFINITE, count = 1, timerIndex = MAXDWORD;
    HANDLE waits[5] = {server_.stoppingHandle()};
    if (scene_.plugin && scene_.plugin->exitHandle())
      waits[count++] = scene_.plugin->exitHandle();
    if (temporaryPauseOwner_)
      waits[count++] = temporaryPauseOwner_;
    if (removed && !deviceRetryAt_ && !monitorSuspendAt_)
      waits[count++] = removed;
    if (plan.timed) {
      if (frameTimer.arm(now, plan.remaining)) {
        timerIndex = count;
        waits[count++] = frameTimer.handle;
      } else {
        highResolutionWait_ = false;
        timeout = FramePacer::ceilingMilliseconds(plan.remaining);
      }
    } else
      frameTimer.cancel();
    const auto wake = MsgWaitForMultipleObjectsEx(count, count ? waits : nullptr, timeout,
                                                  QS_ALLINPUT, MWMO_INPUTAVAILABLE);
    if (timerIndex != MAXDWORD && wake == WAIT_OBJECT_0 + timerIndex)
      frameTimer.armed = false;
    if (wake == WAIT_FAILED) {
      // A broken optional wait handle must not create an unbounded spin.
      frameTimer.cancel();
      if (frameTimer.handle) {
        CloseHandle(frameTimer.handle);
        frameTimer.handle = nullptr;
      }
      highResolutionWait_ = false;
      error_ = "Frame wait failed; using the message-queue fallback.";
      MsgWaitForMultipleObjectsEx(
          0, nullptr, plan.timed ? FramePacer::ceilingMilliseconds(plan.remaining) : INFINITE,
          QS_ALLINPUT, MWMO_INPUTAVAILABLE);
    }
  }
  server_.stop();
  displayGuard_.stop();
  keyboard_.reset();
  effects_.StopAll();
  if (temporaryPauseOwner_)
    CloseHandle(temporaryPauseOwner_);
  if (tray_.hWnd)
    Shell_NotifyIconW(NIM_DELETE, &tray_);
  if (icon_)
    DestroyIcon(icon_);
  return 0;
}
static json selfTest(const Options &o) {
  auto config = PlayerConfig::load(o.root);
  NativeRenderer renderer;
  EffekseerBackend backend;
  std::string error;
  if (!renderer.initialize(nullptr, 1920, 720, true, error))
    throw std::runtime_error(error);
  if (!backend.Initialize(renderer.device(), renderer.context(), 2048))
    throw std::runtime_error(backend.LastError());
  backend.Resize(1920, 720);
  auto sample = o.root / L"vendor-source/windows-native/effekseer/EffekseerForCpp1.80.8/"
                         L"Examples/Resources/Laser01.efkefc";
  int id = backend.LoadEffect(sample);
  if (id < 0)
    throw std::runtime_error(backend.LastError());
  int left = backend.PlayAt(id, 384, 432, 1), right = backend.PlayAt(id, 1536, 288, 1);
  if (left < 0 || right < 0)
    throw std::runtime_error("Cannot create diagnostic effect instances");
  auto background = o.root / L"artifacts/paintball-audit/T_CanvasBG_01.png";
  if (fs::exists(background) && !renderer.loadBackground(background, error))
    throw std::runtime_error(error);
  for (int i = 0; i < 12; ++i) {
    renderer.beginFrame();
    backend.Update(1.0f / 60);
    if (!backend.Draw())
      throw std::runtime_error(backend.LastError());
  }
  fs::path output = o.root / L"artifacts/native-player";
  fs::create_directories(output);
  if (!renderer.saveFrame(output / L"offscreen-diagnostic.png", error))
    throw std::runtime_error(error);
  json report = {{"ok", true},
                 {"renderer", "D3D11 WARP offscreen"},
                 {"windowsShown", 0},
                 {"rawInputRegistered", false},
                 {"calibrationRevision", config.revision},
                 {"calibratedKeys", config.keys.size()},
                 {"diagnosticEffect", "Effekseer vendor Laser01 (not FM Paintball)"},
                 {"activeEffects", backend.ActiveEffectCount()},
                 {"activeParticles", backend.ActiveParticleCount()},
                 {"nativePaintballReady", false},
                 {"image", utf8((output / L"offscreen-diagnostic.png").wstring())}};
  backend.StopAll();
  if (backend.HasActiveEffects())
    throw std::runtime_error("Effects did not stop");
  writeJson(output / L"self-test.json", report);
  return report;
}
int WINAPI wWinMain(HINSTANCE, HINSTANCE, LPWSTR, int) {
  if (const auto helper = KeyboardTaskbar::runHelper())
    return *helper;
  SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
  HRESULT com = CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED);
  HANDLE single = nullptr;
  try {
    auto o = options();
    if (o.importTrial) {
      SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX | SEM_NOOPENFILEERRORBOX);
      const auto resultFile = o.root / L"trial-result.json";
      json result;
      try {
        Player candidate(std::move(o));
        result = candidate.trialWallpaper();
      } catch (const std::exception &error) {
        result = {{"ok", false}, {"error", error.what()}};
      }
      writeJson(resultFile, result);
      if (SUCCEEDED(com))
        CoUninitialize();
      return result.value("ok", false) ? 0 : 1;
    }
    if (!o.command.empty()) {
      auto result = commandForRuntime(o);
      stdoutJson(result);
      if (SUCCEEDED(com))
        CoUninitialize();
      return result.value("ok", false) ? 0 : 1;
    }
    if (o.selfTest) {
      stdoutJson(selfTest(o));
      if (SUCCEEDED(com))
        CoUninitialize();
      return 0;
    }
    if (o.probe) {
      auto config = PlayerConfig::load(o.root);
      json out = {{"calibrationRevision", config.revision},
                  {"calibratedKeys", config.keys.size()},
                  {"devices", json::array()},
                  {"monitors", json::array()}};
      const auto devices = RawKeyboard::enumerate(config.deviceGroup);
      if (!devices)
        throw std::runtime_error("Cannot enumerate keyboard devices");
      for (const auto &d : *devices)
        out["devices"].push_back({{"id", utf8(d.path)}, {"selected", d.selected}});
      for (const auto &m : monitors())
        out["monitors"].push_back({{"device", utf8(m.device)},
                                   {"width", m.bounds.right - m.bounds.left},
                                   {"height", m.bounds.bottom - m.bounds.top},
                                   {"primary", m.primary}});
      stdoutJson({{"ok", true}, {"result", out}});
      if (SUCCEEDED(com))
        CoUninitialize();
      return 0;
    }
    std::wstring name = L"Local\\" + o.pipe;
    single = CreateMutexW(nullptr, TRUE, name.c_str());
    if (!single)
      throw std::runtime_error("Cannot acquire player instance lock");
    if (GetLastError() == ERROR_ALREADY_EXISTS) {
      if (single)
        CloseHandle(single);
      if (SUCCEEDED(com))
        CoUninitialize();
      return 0;
    }
    int result = 0;
    {
      Player app(std::move(o));
      result = app.run();
    }
    if (single)
      CloseHandle(single);
    if (SUCCEEDED(com))
      CoUninitialize();
    return result;
  } catch (const std::exception &e) {
    stdoutJson({{"ok", false}, {"error", e.what()}});
    if (single)
      CloseHandle(single);
    if (SUCCEEDED(com))
      CoUninitialize();
    return 1;
  }
}
