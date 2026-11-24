#pragma once

#include <deque>
#include <optional>
#include <string>
#include <windows.h>
#include <uxtheme.h>

namespace action75 {

class TrayMenu {
public:
  explicit TrayMenu(POINT anchor, std::optional<bool> graphite = std::nullopt);
  ~TrayMenu();
  TrayMenu(const TrayMenu &) = delete;
  TrayMenu &operator=(const TrayMenu &) = delete;

  HMENU root() const { return root_; }
  HMENU createPopup() const;
  bool append(HMENU menu, UINT command, std::wstring label, bool checked = false,
              HMENU submenu = nullptr);
  bool measure(MEASUREITEMSTRUCT &item) const;
  bool draw(const DRAWITEMSTRUCT &item) const;
  std::optional<UINT> show(HWND owner, POINT anchor) const;
  std::optional<POINT> popupAnchor() const { return popupAnchor_; }

private:
  friend class TrayPopup;
  struct Entry {
    std::wstring label;
  };
  bool drawRow(const DRAWITEMSTRUCT &item, HTHEME theme, HPAINTBUFFER buffer,
               wchar_t iconOverride = L'\0') const;
  int scale(int logical) const { return MulDiv(logical, dpi_, 96); }

  UINT dpi_ = 96;
  std::optional<POINT> popupAnchor_;
  COLORREF background_, highlight_, foreground_, selectedText_, separator_;
  bool useNativePopup_ = false;
  bool acrylic_ = false;
  HMENU root_ = nullptr;
  HBRUSH backgroundBrush_ = nullptr;
  HFONT labelFont_ = nullptr;
  HFONT iconFont_ = nullptr;
  std::deque<Entry> entries_;
};

} // namespace action75
