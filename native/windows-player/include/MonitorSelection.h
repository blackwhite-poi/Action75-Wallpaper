#pragma once
#include <string>
#include <vector>
#include <windows.h>

namespace action75 {
struct DisplayIdentity {
  std::wstring path;
  std::wstring serial;
  bool empty() const noexcept { return path.empty() && serial.empty(); }
};

struct Monitor {
  std::wstring device;
  RECT bounds{};
  RECT work{};
  bool primary = false;
  DisplayIdentity identity;
  bool hmd = false;
};

std::vector<Monitor> monitors();
std::wstring displaySerialFromEdid(const std::vector<BYTE> &edid);

// Resolve a physical target. Automatic selection is for a fresh installation.
const Monitor *resolveMonitor(const std::vector<Monitor> &list, const DisplayIdentity &wanted,
                              bool automatic = false);
}
