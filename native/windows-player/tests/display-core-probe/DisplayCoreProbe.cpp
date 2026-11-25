#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <windows.h>
#include <d3d11_4.h>
#include <dxgi1_6.h>
#include <wincodec.h>
#include <windows.devices.display.core.interop.h>

#include <winrt/Windows.Devices.Display.Core.h>
#include <winrt/Windows.Foundation.Collections.h>
#include <winrt/Windows.Graphics.DirectX.Direct3D11.h>

#include <algorithm>
#include <climits>
#include <cmath>
#include <iostream>
#include <limits>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

using namespace winrt;
using namespace Windows::Devices::Display;
using namespace Windows::Devices::Display::Core;
using namespace Windows::Graphics::DirectX;

namespace {
constexpr int Width = 1920;
constexpr int Height = 720;
constexpr auto PixelFormat = DirectXPixelFormat::B8G8R8A8UIntNormalized;

const wchar_t* usageName(DisplayMonitorUsageKind kind) {
  switch (kind) {
  case DisplayMonitorUsageKind::Standard: return L"Standard";
  case DisplayMonitorUsageKind::HeadMounted: return L"HeadMounted";
  case DisplayMonitorUsageKind::SpecialPurpose: return L"SpecialPurpose";
  }
  return L"Unknown";
}

void printDisplayDescriptor(const DisplayMonitor& monitor) {
  const auto size = monitor.NativeResolutionInRawPixels();
  std::wcout << L"  " << monitor.DisplayName().c_str() << L"  " << size.Width << L'x' << size.Height
             << L"\n  " << monitor.DeviceId().c_str();
  try {
    const auto edid = monitor.GetDescriptor(DisplayMonitorDescriptorKind::Edid);
    for (uint32_t base = 128; base + 128 <= edid.size(); base += 128) {
      const auto end = edid[base + 2];
      if (edid[base] != 2 || end < 4 || end > 127) continue;
      for (uint32_t offset = 4; offset < end;) {
        const auto tag = edid[base + offset];
        const auto length = tag & 31;
        if (offset + 1 + length > end) break;
        if ((tag >> 5) == 3 && length >= 4 && edid[base + offset + 1] == 0x5c &&
            edid[base + offset + 2] == 0x12 && edid[base + offset + 3] == 0xca) {
          std::wcout << L"\n  Microsoft EDID VSDB version " << unsigned(edid[base + offset + 4]);
          return;
        }
        offset += 1 + length;
      }
    }
    std::wcout << L"\n  No Microsoft EDID VSDB visible through Display.Core";
  } catch (const hresult_error&) {
    std::wcout << L"\n  EDID unavailable through Display.Core";
  }
}

struct ImageFrame {
  UINT width;
  UINT height;
  std::vector<BYTE> pixels;
};

ImageFrame decodeImage(const wchar_t* file, bool fill) {
  const auto factory = create_instance<IWICImagingFactory>(CLSID_WICImagingFactory);
  com_ptr<IWICBitmapDecoder> decoder;
  check_hresult(factory->CreateDecoderFromFilename(file, nullptr, GENERIC_READ,
                                                   WICDecodeMetadataCacheOnDemand, decoder.put()));
  com_ptr<IWICBitmapFrameDecode> frame;
  check_hresult(decoder->GetFrame(0, frame.put()));
  UINT sourceWidth = 0, sourceHeight = 0;
  check_hresult(frame->GetSize(&sourceWidth, &sourceHeight));
  if (sourceWidth == 0 || sourceHeight == 0) throw hresult_error(E_INVALIDARG, L"Empty image");
  auto source = frame.as<IWICBitmapSource>();
  UINT width = Width;
  UINT height = Height;
  if (fill) {
    if (sourceWidth > INT_MAX || sourceHeight > INT_MAX) throw hresult_error(E_INVALIDARG, L"Image too large");
    const auto cropWidth = static_cast<INT>(std::clamp<uint64_t>(uint64_t(sourceHeight) * Width / Height, 1, sourceWidth));
    const auto cropHeight = static_cast<INT>(std::clamp<uint64_t>(uint64_t(sourceWidth) * Height / Width, 1, sourceHeight));
    const WICRect crop{(static_cast<INT>(sourceWidth) - cropWidth) / 2,
                       (static_cast<INT>(sourceHeight) - cropHeight) / 2, cropWidth, cropHeight};
    com_ptr<IWICBitmapClipper> clipper;
    check_hresult(factory->CreateBitmapClipper(clipper.put()));
    check_hresult(clipper->Initialize(source.get(), &crop));
    source = clipper.as<IWICBitmapSource>();
  } else {
    const double scale = std::min(double(Width) / sourceWidth, double(Height) / sourceHeight);
    width = std::clamp(static_cast<UINT>(std::lround(sourceWidth * scale)), 1u, UINT(Width));
    height = std::clamp(static_cast<UINT>(std::lround(sourceHeight * scale)), 1u, UINT(Height));
  }

  com_ptr<IWICBitmapScaler> scaler;
  check_hresult(factory->CreateBitmapScaler(scaler.put()));
  check_hresult(scaler->Initialize(source.get(), width, height, WICBitmapInterpolationModeFant));
  com_ptr<IWICFormatConverter> converter;
  check_hresult(factory->CreateFormatConverter(converter.put()));
  check_hresult(converter->Initialize(scaler.get(), GUID_WICPixelFormat32bppPBGRA,
                                      WICBitmapDitherTypeNone, nullptr, 0, WICBitmapPaletteTypeCustom));
  ImageFrame image{width, height, std::vector<BYTE>(width * height * 4)};
  check_hresult(converter->CopyPixels(nullptr, width * 4, static_cast<UINT>(image.pixels.size()),
                                     image.pixels.data()));
  return image;
}

void submitTestFrame(const DisplayDevice& device, const DisplayTarget& target, const ImageFrame* image, bool hold) {
  const auto adapterId = target.Adapter().Id();
  const LUID luid{adapterId.LowPart, adapterId.HighPart};
  com_ptr<IDXGIFactory6> factory;
  factory.capture(CreateDXGIFactory2, 0);
  com_ptr<IDXGIAdapter4> adapter;
  adapter.capture(factory, &IDXGIFactory6::EnumAdapterByLuid, luid);

  com_ptr<ID3D11Device> renderDevice;
  com_ptr<ID3D11DeviceContext> context;
  check_hresult(D3D11CreateDevice(adapter.get(), D3D_DRIVER_TYPE_UNKNOWN, nullptr,
                                 D3D11_CREATE_DEVICE_BGRA_SUPPORT, nullptr, 0, D3D11_SDK_VERSION,
                                 renderDevice.put(), nullptr, context.put()));
  const auto fenceDevice = renderDevice.as<ID3D11Device5>();
  const auto interop = device.as<IDisplayDeviceInterop>();
  const auto source = device.CreateScanoutSource(target);
  const DisplayPrimaryDescription desc{Width, Height, PixelFormat, DirectXColorSpace::RgbFullG22NoneP709,
                                       false, {1, 0}};
  const auto primary = device.CreatePrimary(target, desc);
  const auto scanout = device.CreateSimpleScanout(source, primary, 0, 1);
  handle primaryHandle;
  check_hresult(interop->CreateSharedHandle(primary.as<::IInspectable>().get(), nullptr, GENERIC_ALL,
                                            nullptr, primaryHandle.put()));
  com_ptr<ID3D11Texture2D> texture;
  texture.capture(fenceDevice, &ID3D11Device5::OpenSharedResource1, primaryHandle.get());
  com_ptr<ID3D11RenderTargetView> view;
  check_hresult(renderDevice->CreateRenderTargetView(texture.get(), nullptr, view.put()));

  com_ptr<ID3D11Fence> fence;
  fence.capture(fenceDevice, &ID3D11Device5::CreateFence, 0, D3D11_FENCE_FLAG_SHARED);
  handle fenceHandle;
  check_hresult(fence->CreateSharedHandle(nullptr, GENERIC_ALL, nullptr, fenceHandle.put()));
  com_ptr<::IInspectable> importedFence;
  importedFence.capture(interop, &IDisplayDeviceInterop::OpenSharedHandle, fenceHandle.get());

  const float color[]{image ? 0.0f : 0.04f, image ? 0.0f : 0.55f, image ? 0.0f : 0.30f, 1.0f};
  context->ClearRenderTargetView(view.get(), color);
  if (image) {
    D3D11_TEXTURE2D_DESC uploadDesc{};
    uploadDesc.Width = image->width;
    uploadDesc.Height = image->height;
    uploadDesc.MipLevels = 1;
    uploadDesc.ArraySize = 1;
    uploadDesc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
    uploadDesc.SampleDesc.Count = 1;
    const D3D11_SUBRESOURCE_DATA pixels{image->pixels.data(), image->width * 4, 0};
    com_ptr<ID3D11Texture2D> upload;
    check_hresult(renderDevice->CreateTexture2D(&uploadDesc, &pixels, upload.put()));
    context->CopySubresourceRegion(texture.get(), 0, (Width - image->width) / 2,
                                   (Height - image->height) / 2, 0, upload.get(), 0, nullptr);
  }
  check_hresult(context.as<ID3D11DeviceContext4>()->Signal(fence.get(), 1));
  context->Flush();

  const auto pool = device.CreateTaskPool();
  const auto task = pool.CreateTask();
  task.SetScanout(scanout);
  task.SetWait(importedFence.as<DisplayFence>(), 1);
  const auto result = pool.TryExecuteTask(task);
  if (result.PresentStatus() != DisplayPresentStatus::Success) {
    std::wcerr << L"Present status " << static_cast<int>(result.PresentStatus())
               << L", source status " << static_cast<int>(result.SourceStatus()) << L'\n';
    throw hresult_error(E_FAIL, L"Display task was not accepted");
  }
  std::wcout << L"Display driver accepted test frame " << result.PresentId() << std::endl;
  if (hold) {
    std::wcout << L"Image held on Action75. Press Enter in this window to end." << std::endl;
    std::wstring line;
    if (!std::getline(std::wcin, line)) throw hresult_error(E_FAIL, L"Could not read the stop command");
  } else {
    Sleep(image ? 15000 : 2000);
  }
}

int present(const DisplayManager& manager, const DisplayTarget& target, const ImageFrame* image, bool hold) {
  const auto targets = single_threaded_vector<DisplayTarget>({target});
  const auto acquisition = manager.TryAcquireTargetsAndCreateEmptyState(targets);
  if (acquisition.ErrorCode() != DisplayManagerResult::Success) {
    std::wcerr << L"Acquisition status " << static_cast<int>(acquisition.ErrorCode())
               << L", HRESULT 0x" << std::hex << static_cast<uint32_t>(acquisition.ExtendedErrorCode()) << L'\n';
    return 3;
  }
  const auto state = acquisition.State();
  const auto path = state.ConnectTarget(target);
  path.IsInterlaced(false);
  path.Scaling(DisplayPathScaling::Identity);
  path.SourcePixelFormat(PixelFormat);
  DisplayModeInfo best{nullptr};
  double difference = std::numeric_limits<double>::infinity();
  for (const auto& mode : path.FindModes(DisplayModeQueryOptions::None)) {
    const auto size = mode.SourceResolution();
    const auto rate = mode.PresentationRate().VerticalSyncRate;
    if (size.Width != Width || size.Height != Height || rate.Denominator == 0) continue;
    const double delta = std::abs(static_cast<double>(rate.Numerator) / rate.Denominator - 60.0);
    if (delta < difference) {
      difference = delta;
      best = mode;
    }
  }
  if (!best) {
    std::wcerr << L"Target has no compatible 1920x720 mode.\n";
    return 4;
  }
  path.ApplyPropertiesFromMode(best);
  const auto applied = state.TryApply(DisplayStateApplyOptions::FailIfStateChanged);
  if (applied.Status() != DisplayStateOperationStatus::Success) {
    std::wcerr << L"Mode apply status " << static_cast<int>(applied.Status())
               << L", HRESULT 0x" << std::hex << static_cast<uint32_t>(applied.ExtendedErrorCode()) << L'\n';
    return 5;
  }
  const auto rate = best.PresentationRate().VerticalSyncRate;
  std::wcout << L"Applied 1920x720 at " << static_cast<double>(rate.Numerator) / rate.Denominator
             << L" Hz. Submitting " << (image ? L"an image." : L"a green test frame.") << std::endl;
  submitTestFrame(manager.CreateDisplayDevice(target.Adapter()), target, image, hold);
  return 0;
}
} // namespace

void printModes(const DisplayPath& path) {
  using Encoding = DisplayWireFormatPixelEncoding;
  using Depth = DisplayBitsPerChannel;
  using Color = DisplayWireFormatColorSpace;
  using Eotf = DisplayWireFormatEotf;
  using Metadata = DisplayWireFormatHdrMetadata;
  constexpr std::pair<Encoding, const wchar_t*> encodings[]{{Encoding::Rgb444, L"RGB444"},
      {Encoding::Ycc444, L"YCbCr444"}, {Encoding::Ycc422, L"YCbCr422"},
      {Encoding::Ycc420, L"YCbCr420"}, {Encoding::Intensity, L"Intensity"}};
  constexpr std::pair<Depth, int> depths[]{{Depth::Bpc6, 6}, {Depth::Bpc8, 8}, {Depth::Bpc10, 10},
      {Depth::Bpc12, 12}, {Depth::Bpc14, 14}, {Depth::Bpc16, 16}};
  constexpr std::pair<Color, const wchar_t*> colors[]{{Color::BT709, L"BT.709"},
      {Color::BT2020, L"BT.2020"}, {Color::ProfileDefinedWideColorGamut, L"Profile WCG"}};
  constexpr std::pair<Eotf, const wchar_t*> transfers[]{{Eotf::Sdr, L"SDR"}, {Eotf::HdrSmpte2084, L"PQ"}};
  constexpr std::pair<Metadata, const wchar_t*> metadata[]{{Metadata::None, L"none"},
      {Metadata::Hdr10, L"HDR10"}, {Metadata::Hdr10Plus, L"HDR10+"},
      {Metadata::DolbyVisionLowLatency, L"Dolby Vision LL"}};
  const auto modes = path.FindModes(DisplayModeQueryOptions::None);
  std::wcout << modes.Size() << L" mode(s) for the queried path.\n";
  for (const auto& mode : modes) {
    const auto source = mode.SourceResolution();
    const auto target = mode.TargetResolution();
    const auto rate = mode.PresentationRate().VerticalSyncRate;
    std::wcout << L"Source " << source.Width << L'x' << source.Height
               << L", target " << target.Width << L'x' << target.Height << L" @ " << rate.Numerator
               << L'/' << rate.Denominator << L" Hz, source pixel format " << static_cast<int>(mode.SourcePixelFormat())
               << (mode.IsInterlaced() ? L", interlaced" : L", progressive") << L'\n';
    try {
      for (const auto& [encoding, encodingName] : encodings) {
        auto supportedDepths = Depth::Bpc6 | Depth::Bpc8 | Depth::Bpc10 | Depth::Bpc12 | Depth::Bpc14 | Depth::Bpc16;
        try {
          supportedDepths = mode.GetWireFormatSupportedBitsPerChannel(encoding);
        } catch (const hresult_error& error) {
          if (error.code() != E_NOTIMPL) throw;
        }
        for (const auto& [depth, depthValue] : depths) {
          if ((supportedDepths & depth) == Depth::None) continue;
          for (const auto& [color, colorName] : colors) {
            for (const auto& [transfer, transferName] : transfers) {
              for (const auto& [meta, metaName] : metadata) {
                try {
                  const DisplayWireFormat format(encoding, depthValue, color, transfer, meta);
                  if (mode.IsWireFormatSupported(format)) {
                    std::wcout << L"  " << encodingName << L' ' << depthValue << L" bpc " << colorName
                               << L' ' << transferName << L", metadata " << metaName << L'\n';
                  }
                } catch (const hresult_error& error) {
                  if (error.code() != E_INVALIDARG) throw;
                }
              }
            }
          }
        }
      }
    } catch (const hresult_error& error) {
      if (error.code() != E_NOTIMPL) throw;
      std::wcout << L"  Wire format validation unavailable (E_NOTIMPL).\n";
    }
  }
}

int wmain(int argc, wchar_t** argv) {
  const bool list = argc == 1 || (argc == 2 && std::wstring_view(argv[1]) == L"--list");
  const bool readModes = argc == 3 && std::wstring_view(argv[1]) == L"--modes";
  const bool acquireModes = argc == 3 && std::wstring_view(argv[1]) == L"--acquire-modes";
  const bool fillHold = argc == 5 && std::wstring_view(argv[4]) == L"--fill-hold";
  if (!list && !readModes && !acquireModes && ((argc != 3 && argc != 4 && !fillHold) ||
                             std::wstring_view(argv[1]) != L"--present" || !*argv[2])) {
    std::wcerr << L"Usage: action75_display_core_probe [--list | --modes \"<monitor-id>\" | "
                  L"--acquire-modes \"<monitor-id>\" | "
                  L"--present \"<monitor-id>\" [\"<image>\" [--fill-hold]]]\n";
    return 2;
  }
  try {
    init_apartment();
    const auto manager = DisplayManager::Create(DisplayManagerOptions::None);
    const auto targets = manager.GetCurrentTargets();
    DisplayTarget selected{nullptr};
    unsigned connected = 0;
    for (const auto& target : targets) {
      if (!target.IsConnected()) continue;
      ++connected;
      const auto kind = target.UsageKind();
      const auto monitor = target.TryGetMonitor();
      if (list) {
        std::wcout << usageName(kind);
        if (monitor) printDisplayDescriptor(monitor);
        std::wcout << L'\n';
      } else if (monitor && _wcsicmp(monitor.DeviceId().c_str(), argv[2]) == 0) {
        if (selected) {
          std::wcerr << L"Monitor ID is ambiguous.\n";
          return 2;
        }
        selected = target;
      }
    }
    if (list) {
      std::wcout << connected << L" connected target(s).\n";
      return 0;
    }
    if (readModes) {
      if (!selected) throw hresult_error(E_INVALIDARG, L"Monitor not found");
      const auto snapshot = manager.TryReadCurrentStateForAllTargets();
      if (snapshot.ErrorCode() != DisplayManagerResult::Success) {
        std::wcerr << L"Read state status " << static_cast<int>(snapshot.ErrorCode()) << L'\n';
        return 3;
      }
      const auto path = snapshot.State().GetPathForTarget(selected);
      if (!path) throw hresult_error(E_FAIL, L"No active path is visible in the read-only state");
      if (const auto size = path.TargetResolution()) {
        std::wcout << L"Active output " << size.Value().Width << L'x' << size.Value().Height;
        if (const auto rate = path.PresentationRate()) {
          const auto sync = rate.Value().VerticalSyncRate;
          std::wcout << L" @ " << sync.Numerator << L'/' << sync.Denominator << L" Hz";
        }
        std::wcout << L'\n';
      }
      if (const auto format = path.WireFormat()) {
        std::wcout << L"Active wire format: " << format.BitsPerChannel() << L" bpc, encoding "
                   << static_cast<int>(format.PixelEncoding()) << L", color " << static_cast<int>(format.ColorSpace())
                   << L", EOTF " << static_cast<int>(format.Eotf()) << L", HDR metadata "
                   << static_cast<int>(format.HdrMetadata()) << L'\n';
      }
      printModes(path);
      return 0;
    }
    if (!selected || (selected.UsageKind() != DisplayMonitorUsageKind::HeadMounted &&
                      selected.UsageKind() != DisplayMonitorUsageKind::SpecialPurpose)) {
      std::wcerr << L"Monitor must be connected and classified as HeadMounted or SpecialPurpose.\n";
      return 2;
    }
    if (acquireModes) {
      const auto snapshot = manager.TryAcquireTargetsAndCreateEmptyState(single_threaded_vector<DisplayTarget>({selected}));
      if (snapshot.ErrorCode() != DisplayManagerResult::Success) {
        std::wcerr << L"Acquisition status " << static_cast<int>(snapshot.ErrorCode()) << L'\n';
        return 3;
      }
      printModes(snapshot.State().ConnectTarget(selected));
      manager.Close();
      return 0;
    }
    if (fillHold) {
      DWORD inputMode;
      if (!GetConsoleMode(GetStdHandle(STD_INPUT_HANDLE), &inputMode)) {
        throw hresult_error(E_INVALIDARG, L"Fill-and-hold needs an interactive console");
      }
    }
    const auto image = argc >= 4 ? std::make_optional(decodeImage(argv[3], fillHold)) : std::nullopt;
    const int result = present(manager, selected, image ? &*image : nullptr, fillHold);
    manager.Close();
    return result;
  } catch (const hresult_error& error) {
    std::wcerr << L"Display.Core HRESULT 0x" << std::hex << static_cast<uint32_t>(error.code())
               << L": " << error.message().c_str() << L'\n';
    return 1;
  }
}
