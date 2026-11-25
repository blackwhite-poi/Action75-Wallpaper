#include "MonitorSelection.h"
#include <algorithm>
#include <array>
#include <cstdint>
#include <numeric>
#include <setupapi.h>

namespace action75 {
namespace {
template <class Item, class Predicate>
const Item *uniqueMatch(const std::vector<Item> &list, Predicate matches) {
  const Item *result = nullptr;
  for (const auto &item : list) {
    if (!matches(item))
      continue;
    if (result)
      return nullptr;
    result = &item;
  }
  return result;
}

std::wstring serialFromEdid(const std::vector<BYTE> &edid) {
  constexpr std::array<BYTE, 8> header{0, 255, 255, 255, 255, 255, 255, 0};
  if (edid.size() < 128 || !std::equal(header.begin(), header.end(), edid.begin()) ||
      (std::accumulate(edid.begin(), edid.begin() + 128, 0u) & 255u) != 0)
    return {};

  const auto model = (std::uint32_t(edid[8]) << 24) | (std::uint32_t(edid[9]) << 16) |
                     (std::uint32_t(edid[11]) << 8) | edid[10];
  const auto prefix = std::to_wstring(model) + L":";
  for (std::size_t offset = 54; offset + 18 <= 126; offset += 18) {
    const auto *descriptor = edid.data() + offset;
    if (descriptor[0] || descriptor[1] || descriptor[2] || descriptor[3] != 0xff ||
        descriptor[4])
      continue;
    std::wstring text;
    for (std::size_t i = 5; i < 18 && descriptor[i] >= 32 && descriptor[i] < 127; ++i)
      text += static_cast<wchar_t>(descriptor[i]);
    const auto first = text.find_first_not_of(L' ');
    if (first == std::wstring::npos)
      continue;
    text = text.substr(first, text.find_last_not_of(L' ') - first + 1);
    if (text.find_first_not_of(L'0') != std::wstring::npos)
      return prefix + L"text:" + text;
  }
  const auto number = std::uint32_t(edid[12]) | (std::uint32_t(edid[13]) << 8) |
                      (std::uint32_t(edid[14]) << 16) | (std::uint32_t(edid[15]) << 24);
  return number != 0 && number != 0xffffffffu ? prefix + std::to_wstring(number) : std::wstring{};
}

std::wstring monitorSerial(HDEVINFO devices, const std::wstring &path) {
  if (devices == INVALID_HANDLE_VALUE)
    return {};
  SP_DEVICE_INTERFACE_DATA interfaceData{};
  interfaceData.cbSize = sizeof(interfaceData);
  if (!SetupDiOpenDeviceInterfaceW(devices, path.c_str(), 0, &interfaceData))
    return {};
  SP_DEVINFO_DATA deviceData{};
  deviceData.cbSize = sizeof(deviceData);
  DWORD required = 0;
  // The size query also returns the device node for the interface.
  if (!SetupDiGetDeviceInterfaceDetailW(devices, &interfaceData, nullptr, 0, &required,
                                        &deviceData) && GetLastError() != ERROR_INSUFFICIENT_BUFFER)
    return {};
  const auto key = SetupDiOpenDevRegKey(devices, &deviceData, DICS_FLAG_GLOBAL, 0, DIREG_DEV,
                                       KEY_QUERY_VALUE);
  if (key == INVALID_HANDLE_VALUE)
    return {};
  DWORD size = 0;
  std::vector<BYTE> edid;
  if (RegGetValueW(key, nullptr, L"EDID", RRF_RT_REG_BINARY, nullptr, nullptr, &size) ==
          ERROR_SUCCESS && size >= 128 && size <= 32768) {
    edid.resize(size);
    if (RegGetValueW(key, nullptr, L"EDID", RRF_RT_REG_BINARY, nullptr, edid.data(), &size) ==
        ERROR_SUCCESS)
      edid.resize(size);
    else
      edid.clear();
  }
  RegCloseKey(key);
  return serialFromEdid(edid);
}

struct DisplayPath {
  std::wstring source;
  std::wstring target;
};

std::vector<DisplayPath> activePaths() {
  constexpr UINT flags = QDC_ONLY_ACTIVE_PATHS;
  for (unsigned attempt = 0; attempt < 3; ++attempt) {
    UINT32 pathCount = 0, modeCount = 0;
    if (GetDisplayConfigBufferSizes(flags, &pathCount, &modeCount) != ERROR_SUCCESS || !pathCount)
      return {};
    std::vector<DISPLAYCONFIG_PATH_INFO> paths(pathCount);
    std::vector<DISPLAYCONFIG_MODE_INFO> modes(modeCount);
    const auto result = QueryDisplayConfig(flags, &pathCount, paths.data(), &modeCount, modes.data(),
                                           nullptr);
    if (result == ERROR_INSUFFICIENT_BUFFER)
      continue;
    if (result != ERROR_SUCCESS)
      return {};
    std::vector<DisplayPath> out;
    for (UINT32 i = 0; i < pathCount; ++i) {
      const auto &path = paths[i];
      DISPLAYCONFIG_SOURCE_DEVICE_NAME source{};
      source.header = {DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, sizeof(source),
                       path.sourceInfo.adapterId, path.sourceInfo.id};
      if (DisplayConfigGetDeviceInfo(&source.header) != ERROR_SUCCESS)
        return {};
      DISPLAYCONFIG_TARGET_DEVICE_NAME target{};
      target.header = {DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, sizeof(target),
                       path.targetInfo.adapterId, path.targetInfo.id};
      const bool identified = path.targetInfo.targetAvailable &&
                              DisplayConfigGetDeviceInfo(&target.header) == ERROR_SUCCESS;
      // Keep unresolved paths in the list so cloned sources still count as ambiguous.
      out.push_back({source.viewGdiDeviceName, identified ? target.monitorDevicePath : L""});
    }
    return out;
  }
  return {};
}
}

std::wstring displaySerialFromEdid(const std::vector<BYTE> &edid) {
  return serialFromEdid(edid);
}

std::vector<Monitor> monitors() {
  std::vector<Monitor> result;
  if (!EnumDisplayMonitors(nullptr, nullptr,
                          [](HMONITOR h, HDC, LPRECT, LPARAM data) -> BOOL {
                            MONITORINFOEXW info{};
                            info.cbSize = sizeof(info);
                            if (!GetMonitorInfoW(h, &info))
                              return FALSE;
                            reinterpret_cast<std::vector<Monitor> *>(data)->push_back(
                                {info.szDevice, info.rcMonitor, info.rcWork,
                                 (info.dwFlags & MONITORINFOF_PRIMARY) != 0, {}});
                            return TRUE;
                          }, reinterpret_cast<LPARAM>(&result)))
    return {};
  const auto paths = activePaths();
  const auto devices = SetupDiCreateDeviceInfoList(nullptr, nullptr);
  for (auto &monitor : result) {
    const auto *path = uniqueMatch(paths, [&](const auto &p) { return p.source == monitor.device; });
    if (path && !path->target.empty())
      monitor.identity = {path->target, monitorSerial(devices, path->target)};
  }
  if (devices != INVALID_HANDLE_VALUE)
    SetupDiDestroyDeviceInfoList(devices);
  return result;
}

const Monitor *resolveMonitor(const std::vector<Monitor> &list, const DisplayIdentity &wanted,
                              bool automatic) {
  if (!wanted.path.empty()) {
    const auto *match = uniqueMatch(list, [&](const Monitor &m) {
      return m.identity.path == wanted.path &&
             (wanted.serial.empty() || m.identity.serial.empty() || m.identity.serial == wanted.serial);
    });
    if (match)
      return match->primary ? nullptr : match;
  }
  if (!wanted.serial.empty()) {
    const auto *match = uniqueMatch(list, [&](const Monitor &m) {
      return !m.identity.path.empty() && m.identity.serial == wanted.serial;
    });
    return match && !match->primary ? match : nullptr;
  }
  if (wanted.empty() && automatic) {
    const auto *match = uniqueMatch(list, [](const Monitor &m) {
      return !m.primary && m.bounds.right - m.bounds.left == 1920 &&
             m.bounds.bottom - m.bounds.top == 720;
    });
    return match && !match->identity.path.empty() ? match : nullptr;
  }
  return nullptr;
}
}
