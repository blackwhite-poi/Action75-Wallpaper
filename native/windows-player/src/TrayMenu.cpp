#include "TrayMenu.h"

#include <algorithm>
#include <dwmapi.h>
#include <shellscalingapi.h>
#include <string_view>
#include <utility>
#include <vector>
#include <windowsx.h>

namespace action75 {
namespace {
constexpr auto Personalize = L"Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize";
constexpr UINT_PTR AnimationTimer = 1, OpenSubmenuTimer = 2, CloseSubmenuTimer = 3;

struct Taskbar {
  HMONITOR monitor;
  RECT bounds{};
  HWND window = nullptr;
};

BOOL CALLBACK findTaskbar(HWND window, LPARAM user) {
  auto &bar = *reinterpret_cast<Taskbar *>(user);
  if (MonitorFromWindow(window, MONITOR_DEFAULTTONULL) != bar.monitor) return TRUE;
  wchar_t name[64]{};
  GetClassNameW(window, name, ARRAYSIZE(name));
  if (std::wstring_view(name) != L"Shell_TrayWnd" &&
      std::wstring_view(name) != L"Shell_SecondaryTrayWnd") return TRUE;
  if (!GetWindowRect(window, &bar.bounds)) return TRUE;
  bar.window = window;
  return FALSE;
}

struct OverflowHit {
  POINT click;
  std::optional<LONG> top;
  bool xaml = false;
};

BOOL CALLBACK findOverflow(HWND window, LPARAM user) {
  if (!IsWindowVisible(window)) return TRUE;
  wchar_t name[64]{};
  GetClassNameW(window, name, ARRAYSIZE(name));
  const std::wstring_view type(name);
  if (type != L"TopLevelWindowForOverflowXamlIsland" && type != L"NotifyIconOverflowWindow")
    return TRUE;

  DWORD cloaked = 0;
  if (SUCCEEDED(DwmGetWindowAttribute(window, DWMWA_CLOAKED, &cloaked, sizeof(cloaked))) && cloaked)
    return TRUE;
  RECT bounds{};
  if (FAILED(DwmGetWindowAttribute(window, DWMWA_EXTENDED_FRAME_BOUNDS, &bounds, sizeof(bounds))) ||
      IsRectEmpty(&bounds)) {
    if (!GetWindowRect(window, &bounds)) return TRUE;
  }
  auto &hit = *reinterpret_cast<OverflowHit *>(user);
  if (!PtInRect(&bounds, hit.click)) return TRUE;
  hit.top = bounds.top;
  hit.xaml = type == L"TopLevelWindowForOverflowXamlIsland";
  return FALSE;
}

bool enablePopupAcrylic(HWND window) {
  struct AccentPolicy { int state; DWORD flags, tint, animation; };
  struct CompositionAttribute { int attribute; AccentPolicy *data; SIZE_T size; };
  using SetComposition = BOOL (WINAPI *)(HWND, const CompositionAttribute *);
  const auto set = reinterpret_cast<SetComposition>(
      GetProcAddress(GetModuleHandleW(L"user32.dll"), "SetWindowCompositionAttribute"));
  const BOOL dark = TRUE;
  DwmSetWindowAttribute(window, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark, sizeof(dark));
  const auto noBackdrop = DWMSBT_NONE;
  DwmSetWindowAttribute(window, DWMWA_SYSTEMBACKDROP_TYPE, &noBackdrop, sizeof(noBackdrop));
  AccentPolicy accent{4, 0, 0xA0202020, 0};
  const CompositionAttribute material{19, &accent, sizeof(accent)};
  const bool accented = set && set(window, &material);
  const auto acrylic = DWMSBT_TRANSIENTWINDOW;
  const bool backdrop = accented ||
      SUCCEEDED(DwmSetWindowAttribute(window, DWMWA_SYSTEMBACKDROP_TYPE, &acrylic, sizeof(acrylic)));
  if (!backdrop) return false;
  const MARGINS margins{-1, -1, -1, -1};
  return SUCCEEDED(DwmExtendFrameIntoClientArea(window, &margins));
}

void drawText(HDC dc, HTHEME theme, const wchar_t *text, int length, RECT bounds,
              UINT flags, COLORREF color) {
  if (theme) {
    DTTOPTS options{sizeof(options)};
    options.dwFlags = DTT_COMPOSITED | DTT_TEXTCOLOR;
    options.crText = color;
    if (SUCCEEDED(DrawThemeTextEx(theme, dc, 0, 0, text, length, flags, &bounds, &options))) return;
  }
  SetBkMode(dc, TRANSPARENT);
  SetTextColor(dc, color);
  DrawTextW(dc, text, length, &bounds, flags);
}

// The DIB is premultiplied for DWM. Fill the actual rounded shape so the
// pixels outside the highlight continue to reveal the backdrop.
void fillRoundedHighlight(HPAINTBUFFER buffer, RECT row, int radius, COLORREF color) {
  RGBQUAD *pixels = nullptr;
  int stride = 0;
  if (FAILED(GetBufferedPaintBits(buffer, &pixels, &stride)) || !pixels) return;
  if (row.right <= row.left || row.bottom <= row.top) return;
  radius = std::min({radius, int((row.right - row.left - 1) / 2),
                    int((row.bottom - row.top - 1) / 2)});
  const int r = 4 * radius;
  for (LONG y = row.top; y < row.bottom; ++y) {
    for (LONG x = row.left; x < row.right; ++x) {
      int coverage = 0;
      for (const int sy : {1, 3}) {
        for (const int sx : {1, 3}) {
          const LONG px = 4 * x + sx, py = 4 * y + sy;
          const LONG dx = px - std::clamp(px, 4 * row.left + r, 4 * row.right - r);
          const LONG dy = py - std::clamp(py, 4 * row.top + r, 4 * row.bottom - r);
          coverage += dx * dx + dy * dy <= r * r;
        }
      }
      if (!coverage) continue;
      auto &pixel = pixels[y * stride + x];
      const int alpha = 255 * coverage / 4;
      const auto over = [alpha](BYTE foreground, BYTE existing) {
        return static_cast<BYTE>((foreground * alpha + existing * (255 - alpha)) / 255);
      };
      pixel = {over(GetBValue(color), pixel.rgbBlue), over(GetGValue(color), pixel.rgbGreen),
               over(GetRValue(color), pixel.rgbRed), over(255, pixel.rgbReserved)};
    }
  }
}
}

TrayMenu::TrayMenu(POINT anchor, std::optional<bool> graphite) {
  const auto monitor = MonitorFromPoint(anchor, MONITOR_DEFAULTTONEAREST);
  // Snapshot shell geometry before building the menu or taking focus.
  Taskbar bar{monitor};
  EnumWindows(findTaskbar, reinterpret_cast<LPARAM>(&bar));
  OverflowHit hit{anchor};
  EnumWindows(findOverflow, reinterpret_cast<LPARAM>(&hit));
  // FSA uses the taskbar's DPI. Explorer can retain the flyout HWND and
  // its old DPI when the desktop scale changes.
  dpi_ = bar.window ? GetDpiForWindow(bar.window) : 0;
  if (!dpi_) {
    UINT dpiY = 0;
    if (FAILED(GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &dpi_, &dpiY)) || !dpi_)
      dpi_ = 96;
  }
  if (hit.top) {
    // The Windows 11 XAML window bounds include a transparent 12-DIP gutter.
    // The gap is measured from the painted panel inside that window.
    const int inset = hit.xaml ? scale(12) : 0;
    popupAnchor_ = POINT{anchor.x, *hit.top + inset - scale(6)};
  } else {
    MONITORINFO info{sizeof(info)};
    if (GetMonitorInfoW(monitor, &info)) {
      const auto &rect = bar.bounds;
      // Match FSA's ordinary tray placement, including the work-area fallback.
      const bool bottomBar = bar.window && rect.right - rect.left > rect.bottom - rect.top &&
                             rect.top > info.rcMonitor.top;
      popupAnchor_ = POINT{anchor.x, (bottomBar ? rect.top : info.rcWork.bottom) - scale(6)};
    }
  }

  DWORD light = 1, size = sizeof(light);
  RegGetValueW(HKEY_CURRENT_USER, Personalize, L"SystemUsesLightTheme", RRF_RT_REG_DWORD,
               nullptr, &light, &size);
  if (graphite) light = !*graphite;
  HIGHCONTRASTW contrast{sizeof(contrast)};
  const bool highContrast =
      SystemParametersInfoW(SPI_GETHIGHCONTRAST, sizeof(contrast), &contrast, 0) &&
      (contrast.dwFlags & HCF_HIGHCONTRASTON);
  BOOL screenReader = FALSE;
  SystemParametersInfoW(SPI_GETSCREENREADER, 0, &screenReader, 0);
  useNativePopup_ = highContrast || screenReader;
  DWORD transparency = 1;
  size = sizeof(transparency);
  RegGetValueW(HKEY_CURRENT_USER, Personalize, L"EnableTransparency", RRF_RT_REG_DWORD,
               nullptr, &transparency, &size);
  acrylic_ = !light && transparency && !useNativePopup_;
  background_ = light ? RGB(224, 224, 224) : RGB(22, 22, 22);
  highlight_ = light ? RGB(192, 192, 192) : RGB(62, 62, 62);
  foreground_ = light ? RGB(25, 25, 25) : RGB(243, 243, 243);
  separator_ = light ? RGB(210, 210, 210) : RGB(70, 70, 70);
  selectedText_ = foreground_;
  if (highContrast) {
    background_ = GetSysColor(COLOR_MENU);
    highlight_ = GetSysColor(COLOR_HIGHLIGHT);
    foreground_ = GetSysColor(COLOR_MENUTEXT);
    selectedText_ = GetSysColor(COLOR_HIGHLIGHTTEXT);
    separator_ = GetSysColor(COLOR_MENUTEXT);
  }

  backgroundBrush_ = CreateSolidBrush(background_);
  root_ = createPopup();
  labelFont_ = CreateFontW(-scale(14), 0, 0, 0, FW_NORMAL, FALSE, FALSE, FALSE,
                          DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS,
                          CLEARTYPE_QUALITY, DEFAULT_PITCH, L"Segoe UI");
  iconFont_ = CreateFontW(-scale(14), 0, 0, 0, FW_NORMAL, FALSE, FALSE, FALSE,
                         DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS,
                         ANTIALIASED_QUALITY, DEFAULT_PITCH, L"Segoe Fluent Icons");
}

TrayMenu::~TrayMenu() {
  if (root_) DestroyMenu(root_);
  if (labelFont_) DeleteObject(labelFont_);
  if (iconFont_) DeleteObject(iconFont_);
  if (backgroundBrush_) DeleteObject(backgroundBrush_);
}

HMENU TrayMenu::createPopup() const {
  HMENU menu = CreatePopupMenu();
  if (menu && backgroundBrush_) {
    MENUINFO appearance{sizeof(appearance)};
    appearance.fMask = MIM_BACKGROUND;
    appearance.hbrBack = backgroundBrush_;
    SetMenuInfo(menu, &appearance);
  }
  return menu;
}

bool TrayMenu::append(HMENU menu, UINT command, std::wstring label, bool checked,
                      HMENU submenu) {
  auto &entry = entries_.emplace_back(Entry{std::move(label)});
  MENUITEMINFOW info{sizeof(info)};
  info.fMask = MIIM_ID | MIIM_STRING | MIIM_FTYPE | MIIM_STATE | MIIM_DATA;
  info.fType = MFT_OWNERDRAW;
  info.fState = checked ? MFS_CHECKED : MFS_UNCHECKED;
  info.wID = command;
  info.dwTypeData = entry.label.data();
  info.dwItemData = reinterpret_cast<ULONG_PTR>(&entry);
  if (submenu) {
    info.fMask |= MIIM_SUBMENU;
    info.hSubMenu = submenu;
  }
  if (InsertMenuItemW(menu, static_cast<UINT>(GetMenuItemCount(menu)), TRUE, &info))
    return true;
  entries_.pop_back();
  return false;
}

bool TrayMenu::measure(MEASUREITEMSTRUCT &item) const {
  if (item.CtlType != ODT_MENU || !item.itemData) return false;
  const auto &entry = *reinterpret_cast<const Entry *>(item.itemData);
  SIZE text{};
  if (HDC dc = GetDC(nullptr)) {
    const auto old = SelectObject(dc, labelFont_ ? labelFont_ : GetStockObject(DEFAULT_GUI_FONT));
    GetTextExtentPoint32W(dc, entry.label.c_str(), static_cast<int>(entry.label.size()), &text);
    SelectObject(dc, old);
    ReleaseDC(nullptr, dc);
  }
  item.itemWidth = std::clamp<int>(text.cx + scale(60), scale(195), scale(365));
  item.itemHeight = std::max<int>(scale(32), text.cy + scale(11));
  return true;
}

bool TrayMenu::draw(const DRAWITEMSTRUCT &item) const {
  return drawRow(item, nullptr, nullptr);
}

bool TrayMenu::drawRow(const DRAWITEMSTRUCT &item, HTHEME theme, HPAINTBUFFER buffer,
                      wchar_t iconOverride) const {
  if (item.CtlType != ODT_MENU || !item.itemData) return false;
  const auto &entry = *reinterpret_cast<const Entry *>(item.itemData);
  const HDC dc = item.hDC;
  const int restore = SaveDC(dc);
  const bool selected = (item.itemState & ODS_SELECTED) != 0;
  if (!theme && backgroundBrush_)
    FillRect(dc, &item.rcItem, backgroundBrush_);
  if (selected && buffer) {
    if (!theme) BufferedPaintSetAlpha(buffer, &item.rcItem, 255);
    fillRoundedHighlight(buffer, item.rcItem, scale(5), highlight_);
  } else if (selected) {
    const HBRUSH brush = CreateSolidBrush(highlight_);
    SelectObject(dc, brush);
    SelectObject(dc, GetStockObject(NULL_PEN));
    RoundRect(dc, item.rcItem.left, item.rcItem.top,
              item.rcItem.right, item.rcItem.bottom, scale(10), scale(10));
    SelectObject(dc, GetStockObject(NULL_BRUSH));
    DeleteObject(brush);
  }
  SetBkMode(dc, TRANSPARENT);
  const bool checked = (item.itemState & ODS_CHECKED) != 0;
  const wchar_t glyph[]{checked ? L'\uE73E' : iconOverride, L'\0'};
  if (glyph[0] && iconFont_) {
    RECT icon = item.rcItem;
    icon.left += scale(8);
    icon.right = icon.left + scale(24);
    SelectObject(dc, iconFont_);
    drawText(dc, theme, glyph, 1, icon, DT_SINGLELINE | DT_CENTER | DT_VCENTER | DT_NOPREFIX,
             selected ? selectedText_ : foreground_);
  }
  RECT label = item.rcItem;
  label.left += scale(36);
  label.right -= scale(8);
  SelectObject(dc, labelFont_ ? labelFont_ : GetStockObject(DEFAULT_GUI_FONT));
  drawText(dc, theme, entry.label.c_str(), static_cast<int>(entry.label.size()), label,
           DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
           selected ? selectedText_ : foreground_);
  RestoreDC(dc, restore);
  return true;
}

class TrayPopup {
public:
  explicit TrayPopup(const TrayMenu &menu) : menu_(menu) {}
  ~TrayPopup() {
    if (buffered_) BufferedPaintUnInit();
  }

  std::optional<UINT> run(HWND owner, POINT anchor) {
    MONITORINFO monitor{sizeof(monitor)};
    const auto monitorHandle = MonitorFromPoint(anchor, MONITOR_DEFAULTTONEAREST);
    if (!GetMonitorInfoW(monitorHandle, &monitor))
      return std::nullopt;
    work_ = monitor.rcWork;
    anchor = menu_.popupAnchor().value_or(anchor);
    buffered_ = SUCCEEDED(BufferedPaintInit());
    if (!open(root_, owner, menu_.root(), anchor)) return std::nullopt;
    reveal(root_, SW_SHOW);
    SetForegroundWindow(root_.window);
    SetFocus(root_.window);
    SetCapture(root_.window);
    MSG message{};
    while (!done_) {
      const BOOL result = GetMessageW(&message, nullptr, 0, 0);
      if (result <= 0) {
        if (!result) PostQuitMessage(static_cast<int>(message.wParam));
        break;
      }
      TranslateMessage(&message);
      DispatchMessageW(&message);
    }
    if (GetCapture() == root_.window) ReleaseCapture();
    closeChild();
    DestroyWindow(root_.window);
    return choice_;
  }

private:
  struct Row {
    UINT command;
    HMENU submenu;
    ULONG_PTR data;
    UINT state;
  };
  struct Panel {
    TrayPopup *session = nullptr;
    HWND window = nullptr;
    HTHEME textTheme = nullptr;
    RECT bounds{};
    RECT visibleBounds{};
    std::vector<Row> rows;
    int first = 0, slots = 0, selected = -1;
    ULONGLONG animationStart = 0;
  };

  int scale(int n) const { return menu_.scale(n); }
  int padding() const { return scale(8); }
  int rowPitch() const { return scale(34); }
  int itemHeight() const { return scale(32); }

  int rowTop(const Panel &panel, int row) const {
    int y = padding();
    for (int i = panel.first; i < row; ++i)
      y += panel.rows[i].data ? rowPitch() : scale(9);
    return y;
  }

  bool open(Panel &panel, HWND owner, HMENU items, POINT anchor) {
    const HINSTANCE instance = GetModuleHandleW(nullptr);
    constexpr auto className = L"Action75TrayPopup";
    WNDCLASSW wc{};
    wc.hInstance = instance;
    wc.lpfnWndProc = procedure;
    wc.hCursor = LoadCursorW(nullptr, IDC_ARROW);
    wc.lpszClassName = className;
    if (!RegisterClassW(&wc) && GetLastError() != ERROR_CLASS_ALREADY_EXISTS)
      return false;
    panel = {};
    panel.session = this;
    const int count = GetMenuItemCount(items);
    for (int i = 0; i < count; ++i) {
      MENUITEMINFOW item{sizeof(item)};
      item.fMask = MIIM_ID | MIIM_SUBMENU | MIIM_DATA | MIIM_STATE | MIIM_FTYPE;
      if (!GetMenuItemInfoW(items, i, TRUE, &item)) continue;
      const auto data = item.fType & MFT_SEPARATOR ? 0 : item.dwItemData;
      panel.rows.push_back({item.wID, item.hSubMenu, data, item.fState});
    }
    const int areaWidth = work_.right - work_.left;
    const int areaHeight = work_.bottom - work_.top;
    panel.slots = std::min(static_cast<int>(panel.rows.size()),
                           (areaHeight - 2 * padding()) / rowPitch());
    if (panel.slots <= 0 || areaWidth <= 0) return false;
    const int width = std::min(scale(200), areaWidth);
    const bool scrolls = panel.slots < static_cast<int>(panel.rows.size());
    const int height = scrolls ? panel.slots * rowPitch() + 2 * padding()
                               : rowTop(panel, panel.slots) + scale(6);
    const bool child = &panel == &child_;
    int x = anchor.x - width, y = anchor.y - height;
    if (child && x < work_.left) x = root_.bounds.right + scale(4);
    x = std::clamp(x, static_cast<int>(work_.left), static_cast<int>(work_.right - width));
    y = std::clamp(y, static_cast<int>(work_.top), static_cast<int>(work_.bottom - height));
    panel.bounds = {x, y, x + width, y + height};
    panel.visibleBounds = panel.bounds;
    panel.window = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_TOPMOST | (child ? WS_EX_NOACTIVATE : 0),
                                   className, L"Action75 · 托盘菜单", WS_POPUP,
                                   x, y, width, height, owner, nullptr, instance, &panel);
    if (!panel.window) return false;
    const auto corners = DWMWCP_ROUND;
    DwmSetWindowAttribute(panel.window, DWMWA_WINDOW_CORNER_PREFERENCE, &corners, sizeof(corners));
    const COLORREF border = DWMWA_COLOR_NONE;
    DwmSetWindowAttribute(panel.window, DWMWA_BORDER_COLOR, &border, sizeof(border));
    if (buffered_ && menu_.acrylic_) {
      panel.textTheme = OpenThemeData(panel.window, L"WINDOW");
      if (panel.textTheme && !enablePopupAcrylic(panel.window)) {
        CloseThemeData(panel.textTheme);
        panel.textTheme = nullptr;
      }
    }
    return true;
  }

  void animate(Panel &panel) {
    constexpr double duration = 180.;
    const double remaining = std::max(0., 1. - (GetTickCount64() - panel.animationStart) / duration);
    const int offset = static_cast<int>(scale(24) * remaining * remaining * remaining);
    panel.visibleBounds = panel.bounds;
    OffsetRect(&panel.visibleBounds, 0, offset);
    SetWindowPos(panel.window, nullptr, panel.visibleBounds.left, panel.visibleBounds.top, 0, 0,
                 SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
    if (remaining == 0.) KillTimer(panel.window, AnimationTimer);
  }

  void reveal(Panel &panel, int mode) {
    BOOL animateMenu = TRUE;
    SystemParametersInfoW(SPI_GETMENUANIMATION, 0, &animateMenu, 0);
    const BOOL disabled = TRUE;
    DwmSetWindowAttribute(panel.window, DWMWA_TRANSITIONS_FORCEDISABLED, &disabled, sizeof(disabled));
    if (animateMenu && SetTimer(panel.window, AnimationTimer, 16, nullptr)) {
      panel.animationStart = GetTickCount64();
      animate(panel);
    }
    RedrawWindow(panel.window, nullptr, nullptr, RDW_INVALIDATE | RDW_UPDATENOW);
    ShowWindow(panel.window, mode);
    UpdateWindow(panel.window);
  }

  void closeChild() {
    KillTimer(root_.window, CloseSubmenuTimer);
    if (child_.window) DestroyWindow(child_.window);
    child_ = {};
    submenuRow_ = -1;
  }

  void openChild(int row) {
    if (child_.window || !isAction(root_, row) || !root_.rows[row].submenu) return;
    KillTimer(root_.window, OpenSubmenuTimer);
    pendingSubmenu_ = -1;
    const LONG entryTop = root_.bounds.top + rowTop(root_, row);
    const POINT anchor{root_.bounds.left - scale(4), entryTop + itemHeight()};
    if (open(child_, root_.window, root_.rows[row].submenu, anchor)) {
      submenuRow_ = row;
      const RECT entry{root_.bounds.left, entryTop, root_.bounds.right, entryTop + itemHeight()};
      submenuOrigin_ = PtInRect(&entry, pointer_)
          ? pointer_ : POINT{(entry.left + entry.right) / 2, (entry.top + entry.bottom) / 2};
      reveal(child_, SW_SHOWNOACTIVATE);
      InvalidateRect(root_.window, nullptr, FALSE);
    }
  }

  bool movingTowardSubmenu(POINT point) const {
    const LONG edge = child_.bounds.right <= root_.bounds.left
                          ? child_.bounds.right : child_.bounds.left;
    const POINT top{edge, child_.bounds.top - scale(12)};
    const POINT bottom{edge, child_.bounds.bottom + scale(12)};
    const auto side = [](POINT a, POINT b, POINT c) {
      return LONGLONG(b.x - a.x) * (c.y - a.y) - LONGLONG(b.y - a.y) * (c.x - a.x);
    };
    const auto a = side(submenuOrigin_, top, point);
    const auto b = side(top, bottom, point);
    const auto c = side(bottom, submenuOrigin_, point);
    return (a >= 0 && b >= 0 && c >= 0) || (a <= 0 && b <= 0 && c <= 0);
  }

  bool isAction(const Panel &panel, int row) const {
    return row >= 0 && row < static_cast<int>(panel.rows.size()) &&
           panel.rows[row].data && !(panel.rows[row].state & MFS_DISABLED);
  }

  int hit(const Panel &panel, POINT point) const {
    if (!panel.window || !PtInRect(&panel.visibleBounds, point)) return -1;
    if (point.x < panel.visibleBounds.left + padding() ||
        point.x >= panel.visibleBounds.right - padding()) return -1;
    const int y = point.y - panel.visibleBounds.top;
    for (int row = panel.first; row < panel.first + panel.slots; ++row) {
      const int top = rowTop(panel, row);
      if (isAction(panel, row) && y >= top && y < top + itemHeight()) return row;
    }
    return -1;
  }

  void select(Panel &panel, int row) {
    if (panel.selected == row) return;
    panel.selected = row;
    if (row >= 0)
      panel.first = std::clamp(panel.first, std::max(0, row - panel.slots + 1), row);
    InvalidateRect(panel.window, nullptr, FALSE);
  }

  void move(Panel &panel, int direction) {
    const int count = static_cast<int>(panel.rows.size());
    int row = panel.selected < 0 && direction < 0 ? 0 : panel.selected;
    for (int i = 0; i < count; ++i) {
      row = (row + direction + count) % count;
      if (isAction(panel, row)) {
        select(panel, row);
        return;
      }
    }
  }

  void pointer(POINT point, bool choose) {
    pointer_ = point;
    Panel &panel = child_.window && PtInRect(&child_.visibleBounds, point) ? child_ : root_;
    const int row = hit(panel, point);
    KillTimer(root_.window, CloseSubmenuTimer);
    if (&panel == &root_ && child_.window) {
      if (row == submenuRow_) submenuOrigin_ = point;
      else if (!choose && (row < 0 || movingTowardSubmenu(point))) {
        SetTimer(root_.window, CloseSubmenuTimer, 600, nullptr);
        return;
      } else closeChild();
    }
    if (&panel != &root_ || row != pendingSubmenu_) {
      KillTimer(root_.window, OpenSubmenuTimer);
      pendingSubmenu_ = -1;
    }
    if (row < 0) {
      select(panel, -1);
      if (choose && !PtInRect(&panel.visibleBounds, point)) done_ = true;
      return;
    }
    select(panel, row);
    if (panel.rows[row].submenu) {
      if (choose) openChild(row);
      else if (!child_.window && pendingSubmenu_ != row) {
        UINT delay = 400;
        SystemParametersInfoW(SPI_GETMENUSHOWDELAY, 0, &delay, 0);
        if (SetTimer(root_.window, OpenSubmenuTimer, std::clamp(delay, 300u, 1000u), nullptr))
          pendingSubmenu_ = row;
      }
    } else if (choose) {
      choice_ = panel.rows[row].command;
      done_ = true;
    }
  }

  void key(WPARAM key) {
    Panel &panel = child_.window ? child_ : root_;
    switch (key) {
    case VK_DOWN: move(panel, 1); break;
    case VK_UP: move(panel, -1); break;
    case VK_ESCAPE:
    case VK_LEFT:
      if (child_.window) closeChild();
      else if (key == VK_ESCAPE) done_ = true;
      break;
    case VK_RIGHT:
    case VK_RETURN:
    case VK_SPACE: {
      const int row = panel.selected;
      if (!isAction(panel, row)) break;
      if (panel.rows[row].submenu) {
        openChild(row);
        if (child_.window) move(child_, 1);
      } else if (key != VK_RIGHT) {
        choice_ = panel.rows[row].command;
        done_ = true;
      }
      break;
    }
    }
  }

  void paint(Panel &panel) const {
    PAINTSTRUCT ps{};
    const HDC target = BeginPaint(panel.window, &ps);
    HDC dc = target;
    RECT client{};
    GetClientRect(panel.window, &client);
    BP_PAINTPARAMS params{sizeof(params)};
    params.dwFlags = BPPF_ERASE;
    HPAINTBUFFER buffer = buffered_
        ? BeginBufferedPaint(target, &client, BPBF_TOPDOWNDIB, &params, &dc) : nullptr;
    const auto theme = buffer ? panel.textTheme : nullptr;
    if (theme) {
      BufferedPaintClear(buffer, &client);
      RGBQUAD *pixels = nullptr;
      int stride = 0;
      if (SUCCEEDED(GetBufferedPaintBits(buffer, &pixels, &stride)) && pixels) {
        const RGBQUAD tint{0, 0, 0, 96};
        for (LONG y = client.top; y < client.bottom; ++y)
          std::fill_n(pixels + y * stride, client.right, tint);
      }
    } else {
      if (!buffer) dc = target;
      FillRect(dc, &client, menu_.backgroundBrush_);
    }
    for (int i = 0; i < panel.slots; ++i) {
      const int row = panel.first + i;
      const auto &entry = panel.rows[row];
      const int y = rowTop(panel, row);
      RECT bounds{padding(), y, client.right - padding(), y + itemHeight()};
      if (!entry.data) {
        const int top = y + scale(2);
        const RECT line{bounds.left + padding(), top, bounds.right - padding(), top + scale(1)};
        const HBRUSH brush = CreateSolidBrush(menu_.separator_);
        FillRect(dc, &line, brush);
        DeleteObject(brush);
        if (buffer) BufferedPaintSetAlpha(buffer, &line, 255);
        continue;
      }
      DRAWITEMSTRUCT item{};
      item.CtlType = ODT_MENU;
      item.itemData = entry.data;
      item.hDC = dc;
      item.rcItem = bounds;
      if (entry.state & MFS_CHECKED) item.itemState |= ODS_CHECKED;
      if (row == panel.selected) item.itemState |= ODS_SELECTED;
      const wchar_t arrow = !entry.submenu ? L'\0' :
          (child_.window && child_.bounds.left >= root_.bounds.right ? L'\uE76C' : L'\uE76B');
      menu_.drawRow(item, theme, buffer, arrow);
    }
    if (buffer) {
      if (!theme) BufferedPaintSetAlpha(buffer, &client, 255);
      EndBufferedPaint(buffer, TRUE);
    }
    EndPaint(panel.window, &ps);
  }

  static LRESULT CALLBACK procedure(HWND hwnd, UINT message, WPARAM w, LPARAM l) {
    if (message == WM_NCCREATE) {
      const auto *create = reinterpret_cast<const CREATESTRUCTW *>(l);
      auto *panel = static_cast<Panel *>(create->lpCreateParams);
      panel->window = hwnd;
      SetWindowLongPtrW(hwnd, GWLP_USERDATA, reinterpret_cast<LONG_PTR>(panel));
    }
    auto *panel = reinterpret_cast<Panel *>(GetWindowLongPtrW(hwnd, GWLP_USERDATA));
    if (!panel) return DefWindowProcW(hwnd, message, w, l);
    auto &session = *panel->session;
    switch (message) {
    case WM_NCCALCSIZE: return 0;
    case WM_NCDESTROY:
      if (panel->textTheme) CloseThemeData(std::exchange(panel->textTheme, nullptr));
      break;
    case WM_NCHITTEST: return HTCLIENT;
    case WM_ERASEBKGND: return 1;
    case WM_PAINT: session.paint(*panel); return 0;
    case WM_TIMER:
      if (w == AnimationTimer) { session.animate(*panel); return 0; }
      if (w == OpenSubmenuTimer) {
        KillTimer(hwnd, w);
        const int row = std::exchange(session.pendingSubmenu_, -1);
        if (row == session.hit(session.root_, session.pointer_)) session.openChild(row);
        return 0;
      }
      if (w == CloseSubmenuTimer) {
        KillTimer(hwnd, w);
        if (session.hit(session.root_, session.pointer_) != session.submenuRow_ &&
            !PtInRect(&session.child_.visibleBounds, session.pointer_))
          session.closeChild();
        return 0;
      }
      break;
    case WM_CANCELMODE:
    case WM_CLOSE:
      session.done_ = true;
      return 0;
    case WM_ACTIVATE:
      if (LOWORD(w) == WA_INACTIVE && panel == &session.root_) session.done_ = true;
      break;
    case WM_CAPTURECHANGED:
      if (panel == &session.root_) session.done_ = true;
      break;
    case WM_MOUSEMOVE:
    case WM_LBUTTONUP: {
      POINT point{GET_X_LPARAM(l), GET_Y_LPARAM(l)};
      ClientToScreen(hwnd, &point);
      session.pointer(point, message == WM_LBUTTONUP);
      return 0;
    }
    case WM_LBUTTONDOWN:
    case WM_RBUTTONDOWN: {
      POINT point{GET_X_LPARAM(l), GET_Y_LPARAM(l)};
      ClientToScreen(hwnd, &point);
      if (!PtInRect(&session.root_.visibleBounds, point) &&
          (!session.child_.window || !PtInRect(&session.child_.visibleBounds, point)))
        session.done_ = true;
      return 0;
    }
    case WM_MOUSEWHEEL: {
      POINT point{GET_X_LPARAM(l), GET_Y_LPARAM(l)};
      Panel &target = session.child_.window && PtInRect(&session.child_.visibleBounds, point)
                          ? session.child_ : session.root_;
      const int last = static_cast<int>(target.rows.size()) - target.slots;
      const int first = std::clamp(target.first - GET_WHEEL_DELTA_WPARAM(w) / WHEEL_DELTA, 0, last);
      if (first != target.first) {
        if (&target == &session.root_) session.closeChild();
        target.first = first;
        target.selected = -1;
        InvalidateRect(target.window, nullptr, FALSE);
      }
      return 0;
    }
    case WM_KEYDOWN: session.key(w); return 0;
    }
    return DefWindowProcW(hwnd, message, w, l);
  }

  const TrayMenu &menu_;
  RECT work_{};
  Panel root_, child_;
  POINT pointer_{}, submenuOrigin_{};
  int pendingSubmenu_ = -1, submenuRow_ = -1;
  bool done_ = false;
  bool buffered_ = false;
  UINT choice_ = 0;
};

std::optional<UINT> TrayMenu::show(HWND owner, POINT anchor) const {
  if (useNativePopup_ || !backgroundBrush_) return std::nullopt;
  return TrayPopup(*this).run(owner, anchor);
}

} // namespace action75
