#include "NativeRenderer.h"
#include "EffekseerBackend.h"
#include "VideoBackground.h"
#include <algorithm>
#include <chrono>
#include <cmath>
#include <d3d11_4.h>
#include "BackgroundShaders.h"
#include <dxgi1_6.h>
#include <sstream>
#include <vector>
#include <wincodec.h>

using Microsoft::WRL::ComPtr;
namespace action75 {
NativeRenderer::BackgroundSnapshot::BackgroundSnapshot()=default;
NativeRenderer::BackgroundSnapshot::~BackgroundSnapshot()=default;
NativeRenderer::BackgroundSnapshot::BackgroundSnapshot(BackgroundSnapshot&&) noexcept=default;
NativeRenderer::BackgroundSnapshot& NativeRenderer::BackgroundSnapshot::operator=(BackgroundSnapshot&&) noexcept=default;
NativeRenderer::BackgroundSnapshot NativeRenderer::takeBackground() noexcept {
  BackgroundSnapshot old;
  old.view=std::move(backgroundView_);old.opaque=std::move(opaqueBackdrop_);old.video=std::move(video_);
  old.media=media_;media_=false;
  old.fit=mediaFit_;old.zoom=mediaZoom_;old.panX=mediaPanX_;old.panY=mediaPanY_;
  old.width=backdropWidth_;old.height=backdropHeight_;old.copied=lastBackdropCopy_;
  std::copy_n(backgroundAppearance_, old.appearance.size(), old.appearance.begin());
  backdropWidth_=backdropHeight_=0;lastBackdropCopy_=false;
  return old;
}
void NativeRenderer::restoreBackground(BackgroundSnapshot&& old) noexcept {
  backgroundView_=std::move(old.view);opaqueBackdrop_=std::move(old.opaque);video_=std::move(old.video);
  media_=old.media;
  setMediaPlacement(old.fit,old.zoom,old.panX,old.panY);
  backdropWidth_=old.width;backdropHeight_=old.height;lastBackdropCopy_=old.copied;
  std::copy(old.appearance.begin(), old.appearance.end(), backgroundAppearance_);
  backgroundAppearanceDirty_ = true;
}
NativeRenderer::NativeRenderer() = default;
static std::string hrError(const char *operation, HRESULT hr) {
  std::ostringstream out;
  out << operation << " failed (0x" << std::hex
      << static_cast<unsigned long>(hr) << ")";
  return out.str();
}
NativeRenderer::~NativeRenderer() { releaseResources(); }

void NativeRenderer::releaseResources() noexcept {
  if (deviceRemovedEvent_) {
    ComPtr<ID3D11Device4> notifications;
    if (SUCCEEDED(device_.As(&notifications)))
      notifications->UnregisterDeviceRemoved(deviceRemovedRegistration_);
    CloseHandle(deviceRemovedEvent_);
    deviceRemovedEvent_ = nullptr;
  }
  if (frameReady_) {
    CloseHandle(frameReady_);
    frameReady_ = nullptr;
  }
  clearBackground();
  targetView_.Reset();
  target_.Reset();
  swapChain_.Reset();
  vertexShader_.Reset();
  pixelShader_.Reset();
  backgroundConstants_.Reset();
  sampler_.Reset();
  rasterizer_.Reset();
  // Flush deferred destruction before a replacement flip chain uses this HWND.
  if (context_) {
    context_->ClearState();
    context_->Flush();
  }
  context_.Reset();
  device_.Reset();
  adapterLuid_ = 0;
  adapterName_.clear();
  width_ = height_ = 0;
  presentPending_ = false;
  lastPresentResult_ = S_OK;
}
bool NativeRenderer::initialize(HWND window, unsigned width, unsigned height,
                                bool offscreen, std::string &error, bool hardwareOffscreen,
                                GpuPreference preference, const LUID *requiredAdapter) {
  releaseResources();
  if (!initializeResources(window, width, height, offscreen, error, hardwareOffscreen, preference, requiredAdapter)) {
    releaseResources();
    return false;
  }
  ComPtr<ID3D11Device4> notifications;
  if (SUCCEEDED(device_.As(&notifications))) {
    const HANDLE event = CreateEventW(nullptr, TRUE, FALSE, nullptr);
    if (event) {
      if (SUCCEEDED(notifications->RegisterDeviceRemovedEvent(event, &deviceRemovedRegistration_)))
        deviceRemovedEvent_ = event;
      else
        CloseHandle(event);
    }
  }
  return true;
}

bool NativeRenderer::initializeResources(HWND window, unsigned width, unsigned height,
                                         bool offscreen, std::string &error, bool hardwareOffscreen,
                                         GpuPreference preference, const LUID *requiredAdapter) {
  offscreen_ = offscreen;
  const D3D_FEATURE_LEVEL levels[] = {D3D_FEATURE_LEVEL_11_0};
  const bool warp = offscreen && !hardwareOffscreen;
  constexpr UINT bgraSupport = D3D11_CREATE_DEVICE_BGRA_SUPPORT;
  const UINT flags = bgraSupport | (warp ? 0u : D3D11_CREATE_DEVICE_VIDEO_SUPPORT);
  const auto createDevice = [&](IDXGIAdapter *adapter, D3D_DRIVER_TYPE type) {
    const auto attempt = [&](UINT requestedFlags) {
      return D3D11CreateDevice(adapter, type, nullptr, requestedFlags, levels, 1, D3D11_SDK_VERSION,
                               &device_, nullptr, &context_);
    };
    const HRESULT result = attempt(flags);
    // Allow older drivers to run hardware offscreen scenes without video.
    return FAILED(result) && offscreen && hardwareOffscreen ? attempt(bgraSupport) : result;
  };
  HRESULT hr = E_FAIL;
  ComPtr<IDXGIFactory6> adapterFactory;
  if (requiredAdapter) {
    ComPtr<IDXGIAdapter1> requested;
    hr = CreateDXGIFactory1(IID_PPV_ARGS(&adapterFactory));
    if (SUCCEEDED(hr))
      hr = adapterFactory->EnumAdapterByLuid(*requiredAdapter, IID_PPV_ARGS(&requested));
    if (SUCCEEDED(hr))
      hr = createDevice(requested.Get(), D3D_DRIVER_TYPE_UNKNOWN);
  } else if (!warp && SUCCEEDED(CreateDXGIFactory1(IID_PPV_ARGS(&adapterFactory)))) {
    const auto order = preference == GpuPreference::Integrated ? DXGI_GPU_PREFERENCE_MINIMUM_POWER
                                                               : DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE;
    for (UINT index = 0; ; ++index) {
      ComPtr<IDXGIAdapter1> candidate;
      if (FAILED(adapterFactory->EnumAdapterByGpuPreference(index, order, IID_PPV_ARGS(&candidate))))
        break;
      DXGI_ADAPTER_DESC1 description{};
      if (SUCCEEDED(candidate->GetDesc1(&description)) &&
          !(description.Flags & DXGI_ADAPTER_FLAG_SOFTWARE)) {
        hr = createDevice(candidate.Get(), D3D_DRIVER_TYPE_UNKNOWN);
        if (SUCCEEDED(hr))
          break;
      }
    }
  }
  if (FAILED(hr) && !requiredAdapter)
    hr = createDevice(nullptr, warp ? D3D_DRIVER_TYPE_WARP : D3D_DRIVER_TYPE_HARDWARE);
  if (FAILED(hr)) {
    error = hrError("D3D11CreateDevice", hr);
    return false;
  }
  ComPtr<IDXGIDevice> dxgiDevice;
  ComPtr<IDXGIAdapter> adapter;
  if (SUCCEEDED(device_.As(&dxgiDevice)) && SUCCEEDED(dxgiDevice->GetAdapter(&adapter))) {
    DXGI_ADAPTER_DESC description{};
    if (SUCCEEDED(adapter->GetDesc(&description))) {
      adapterName_ = description.Description;
      adapterLuid_ = (uint64_t(uint32_t(description.AdapterLuid.HighPart)) << 32) |
                     description.AdapterLuid.LowPart;
    }
  }
  if (!offscreen) {
    ComPtr<IDXGIFactory2> factory;
    if (!adapter || FAILED(adapter->GetParent(IID_PPV_ARGS(&factory)))) {
      error = "Cannot access the selected adapter's DXGI factory";
      return false;
    }
    DXGI_SWAP_CHAIN_DESC1 description{};
    description.Width = width;
    description.Height = height;
    description.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
    description.SampleDesc.Count = 1;
    description.BufferUsage = DXGI_USAGE_RENDER_TARGET_OUTPUT;
    description.BufferCount = 2;
    description.SwapEffect = DXGI_SWAP_EFFECT_FLIP_DISCARD;
    description.Flags = DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT;
    hr = factory->CreateSwapChainForHwnd(device_.Get(), window, &description,
                                         nullptr, nullptr, &swapChain_);
    if (FAILED(hr)) {
      error = hrError("CreateSwapChainForHwnd", hr);
      return false;
    }
    factory->MakeWindowAssociation(window, DXGI_MWA_NO_ALT_ENTER);
    ComPtr<IDXGISwapChain2> latencyChain;
    if (SUCCEEDED(swapChain_.As(&latencyChain))) {
      latencyChain->SetMaximumFrameLatency(1);
      frameReady_ = latencyChain->GetFrameLatencyWaitableObject();
    }
  }
  return resize(width, height, error);
}

bool NativeRenderer::initializeForPlugin(ID3D11Device *device, ID3D11DeviceContext *context,
                                         unsigned width, unsigned height, std::string &error) {
  if (!device || !context || !width || !height) {
    error = "Invalid wallpaper output";
    return false;
  }
  device_ = device;
  context_ = context;
  width_ = width;
  height_ = height;
  return true;
}

bool NativeRenderer::setPluginTarget(ID3D11RenderTargetView *target) {
  if (!target) return false;
  if (targetView_.Get() == target) return true;
  ComPtr<ID3D11Resource> resource;
  target->GetResource(&resource);
  ComPtr<ID3D11Texture2D> texture;
  if (FAILED(resource.As(&texture))) return false;
  target_ = std::move(texture);
  targetView_ = target;
  return true;
}

bool NativeRenderer::initializeBackgroundPipeline(std::string &error) {
  if (rasterizer_) return true;
  HRESULT hr;
  hr = device_->CreateVertexShader(BackgroundVS, sizeof(BackgroundVS),
                                   nullptr, &vertexShader_);
  if (FAILED(hr)) {
    error = hrError("CreateVertexShader", hr);
    return false;
  }
  hr = device_->CreatePixelShader(BackgroundPS, sizeof(BackgroundPS),
                                  nullptr, &pixelShader_);
  if (FAILED(hr)) {
    error = hrError("CreatePixelShader", hr);
    return false;
  }
  D3D11_BUFFER_DESC constants{};
  constants.ByteWidth=sizeof(backgroundAppearance_);
  constants.Usage=D3D11_USAGE_DEFAULT;
  constants.BindFlags=D3D11_BIND_CONSTANT_BUFFER;
  D3D11_SUBRESOURCE_DATA initialConstants{};
  initialConstants.pSysMem=backgroundAppearance_;
  hr=device_->CreateBuffer(&constants,&initialConstants,&backgroundConstants_);
  if(FAILED(hr)) {
    error=hrError("Create background appearance buffer",hr);
    return false;
  }
  backgroundAppearanceDirty_=false;
  D3D11_SAMPLER_DESC sample{};
  sample.Filter = D3D11_FILTER_MIN_MAG_MIP_LINEAR;
  sample.AddressU = sample.AddressV = sample.AddressW =
      D3D11_TEXTURE_ADDRESS_CLAMP;
  sample.MaxLOD = D3D11_FLOAT32_MAX;
  hr = device_->CreateSamplerState(&sample, &sampler_);
  if (FAILED(hr)) {
    error = hrError("CreateSamplerState", hr);
    return false;
  }
  D3D11_RASTERIZER_DESC raster{};
  raster.FillMode = D3D11_FILL_SOLID;
  raster.CullMode = D3D11_CULL_NONE;
  raster.DepthClipEnable = TRUE;
  hr = device_->CreateRasterizerState(&raster, &rasterizer_);
  if (FAILED(hr)) {
    error = hrError("CreateRasterizerState", hr);
    return false;
  }
  return true;
}
bool NativeRenderer::resize(unsigned width, unsigned height,
                            std::string &error) {
  if (!width || !height || width > 16384 || height > 16384) {
    error = "Invalid render size";
    return false;
  }
  context_->OMSetRenderTargets(0, nullptr, nullptr);
  targetView_.Reset();
  target_.Reset();
  HRESULT hr;
  if (swapChain_) {
    hr = swapChain_->ResizeBuffers(
        0, width, height, DXGI_FORMAT_UNKNOWN,
        DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT);
    if (FAILED(hr)) {
      error = hrError("ResizeBuffers", hr);
      return false;
    }
    hr = swapChain_->GetBuffer(0, IID_PPV_ARGS(&target_));
  } else {
    D3D11_TEXTURE2D_DESC description{};
    description.Width = width;
    description.Height = height;
    description.MipLevels = description.ArraySize = 1;
    description.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
    description.SampleDesc.Count = 1;
    description.BindFlags =
        D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE;
    hr = device_->CreateTexture2D(&description, nullptr, &target_);
  }
  if (FAILED(hr)) {
    error = hrError("Create render target", hr);
    return false;
  }
  hr = device_->CreateRenderTargetView(target_.Get(), nullptr, &targetView_);
  if (FAILED(hr)) {
    error = hrError("CreateRenderTargetView", hr);
    return false;
  }
  width_ = width;
  height_ = height;
  if(video_ && !video_->Resize(width,height)){error=video_->LastError();return false;}
  return true;
}
bool NativeRenderer::loadBackground(const std::filesystem::path &file,
                                    std::string &error, unsigned maxPixels, bool media) {
  if (!initializeBackgroundPipeline(error)) return false;
  auto extension=file.extension().wstring();
  std::transform(extension.begin(),extension.end(),extension.begin(),::towlower);
  if(extension==L".mp4"||extension==L".m4v"||extension==L".mov"||extension==L".wmv"||extension==L".mkv"||extension==L".webm"){
    auto candidate=std::make_unique<VideoBackground>();
    if(!candidate->Initialize(device_.Get(),context_.Get())||!candidate->Resize(width_,height_)||!candidate->Open(file,media)){
      error=candidate->LastError();return false;
    }
    media_=media;
    video_=std::move(candidate);
    backgroundView_.Reset();
    opaqueBackdrop_.Reset();
    return true;
  }
  ComPtr<IWICImagingFactory> factory;
  ComPtr<IWICBitmapDecoder> decoder;
  ComPtr<IWICBitmapFrameDecode> frame;
  ComPtr<IWICFormatConverter> converter;
  HRESULT hr = CoCreateInstance(CLSID_WICImagingFactory, nullptr,
                                CLSCTX_INPROC_SERVER, IID_PPV_ARGS(&factory));
  if (SUCCEEDED(hr))
    hr = factory->CreateDecoderFromFilename(file.c_str(), nullptr, GENERIC_READ,
                                            WICDecodeMetadataCacheOnLoad,
                                            &decoder);
  if (SUCCEEDED(hr))
    hr = decoder->GetFrame(0, &frame);
  if (SUCCEEDED(hr))
    hr = factory->CreateFormatConverter(&converter);
  if (SUCCEEDED(hr))
    hr = converter->Initialize(frame.Get(), GUID_WICPixelFormat32bppRGBA,
                               WICBitmapDitherTypeNone, nullptr, 0,
                               WICBitmapPaletteTypeCustom);
  if (FAILED(hr)) {
    error = hrError("Decode background image", hr);
    return false;
  }
  UINT width = 0, height = 0;
  hr = converter->GetSize(&width, &height);
  if (FAILED(hr) || !width || !height || width > 16384 || height > 16384 ||
      static_cast<std::size_t>(width) * height > maxPixels) {
    error = "Background dimensions exceed limits";
    return false;
  }
  std::vector<unsigned char> pixels(static_cast<size_t>(width) * height * 4);
  hr = converter->CopyPixels(nullptr, width * 4,
                             static_cast<UINT>(pixels.size()), pixels.data());
  if (FAILED(hr)) {
    error = hrError("Read background pixels", hr);
    return false;
  }
  D3D11_TEXTURE2D_DESC description{};
  description.Width = width;
  description.Height = height;
  description.MipLevels = description.ArraySize = 1;
  description.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
  description.SampleDesc.Count = 1;
  description.Usage = D3D11_USAGE_IMMUTABLE;
  description.BindFlags = D3D11_BIND_SHADER_RESOURCE;
  D3D11_SUBRESOURCE_DATA data{};
  data.pSysMem = pixels.data();
  data.SysMemPitch = width * 4;
  ComPtr<ID3D11Texture2D> texture;
  ComPtr<ID3D11ShaderResourceView> view;
  hr = device_->CreateTexture2D(&description, &data, &texture);
  if (SUCCEEDED(hr))
    hr = device_->CreateShaderResourceView(texture.Get(), nullptr, &view);
  if (FAILED(hr)) {
    error = hrError("Upload background", hr);
    return false;
  }
  bool opaque=true;
  for (std::size_t i=3;i<pixels.size();i+=4) {
    if (pixels[i]!=255) {opaque=false;break;}
  }
  media_=media;
  backgroundView_=view;
  opaqueBackdrop_=opaque?texture:ComPtr<ID3D11Texture2D>{};
  backdropWidth_=width;
  backdropHeight_=height;
  video_.reset();
  return true;
}
// The Canvas pass is isolated from the output target and the interactive
// effect manager. Its GPU texture is the only resource retained by the player.
bool NativeRenderer::loadCanvasBackground(const std::filesystem::path &file,
                                          std::string &error) {
  if (!initializeBackgroundPipeline(error)) return false;
  constexpr unsigned width = 1920, height = 720;
  constexpr int maxCanvasParticles = 256;
  constexpr float canvasTime = 3.0f / 60.0f;

  EffekseerBackend baker;
  if (!baker.Initialize(device_.Get(), context_.Get(), maxCanvasParticles)) {
    error = baker.LastError();
    return false;
  }
  baker.Resize(width, height);
  const int effectId = baker.LoadEffect(file);
  if (effectId < 0 || baker.PlayAt(effectId, width * 0.5f, height * 0.5f) < 0) {
    error = baker.LastError();
    return false;
  }
  // The contract is a frame-zero, noninteractive effect. Advance enough for
  // Effekseer's deferred spawn, without ever running it on the live clock.
  baker.Update(canvasTime);
  if (!baker.HasActiveEffects()) {
    error = "Canvas effect did not survive to the static snapshot";
    return false;
  }

  D3D11_TEXTURE2D_DESC description{};
  description.Width = width;
  description.Height = height;
  description.MipLevels = description.ArraySize = 1;
  description.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
  description.SampleDesc.Count = 1;
  description.BindFlags = D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE;
  ComPtr<ID3D11Texture2D> texture;
  ComPtr<ID3D11RenderTargetView> target;
  ComPtr<ID3D11ShaderResourceView> view;
  HRESULT hr = device_->CreateTexture2D(&description, nullptr, &texture);
  if (SUCCEEDED(hr))
    hr = device_->CreateRenderTargetView(texture.Get(), nullptr, &target);
  if (SUCCEEDED(hr))
    hr = device_->CreateShaderResourceView(texture.Get(), nullptr, &view);
  if (FAILED(hr)) {
    error = hrError("Allocate cached Effekseer canvas", hr);
    return false;
  }

  // Restore the player's output on all paths. The canvas must be unbound
  // before the fullscreen background shader or a distortion material reads it.
  struct RestoreTarget {
    ID3D11DeviceContext *context;
    ComPtr<ID3D11RenderTargetView> renderTarget;
    ComPtr<ID3D11DepthStencilView> depth;
    UINT count = D3D11_VIEWPORT_AND_SCISSORRECT_OBJECT_COUNT_PER_PIPELINE;
    D3D11_VIEWPORT viewports[D3D11_VIEWPORT_AND_SCISSORRECT_OBJECT_COUNT_PER_PIPELINE]{};
    explicit RestoreTarget(ID3D11DeviceContext *c) : context(c) {
      context->OMGetRenderTargets(1, &renderTarget, &depth);
      context->RSGetViewports(&count, viewports);
    }
    ~RestoreTarget() {
      auto *rt = renderTarget.Get();
      context->OMSetRenderTargets(rt ? 1 : 0, rt ? &rt : nullptr, depth.Get());
      context->RSSetViewports(count, viewports);
    }
  };
  {
    RestoreTarget restore(context_.Get());
    auto *rt = target.Get();
    context_->OMSetRenderTargets(1, &rt, nullptr);
    D3D11_VIEWPORT viewport{};
    viewport.Width = static_cast<float>(width);
    viewport.Height = static_cast<float>(height);
    viewport.MaxDepth = 1;
    context_->RSSetViewports(1, &viewport);
    const float black[4] = {0, 0, 0, 1};
    context_->ClearRenderTargetView(rt, black);
    if (!baker.Draw()) {
      error = baker.LastError();
      return false;
    }
  }
  if (FAILED(hr = device_->GetDeviceRemovedReason())) {
    error = hrError("Render cached Effekseer canvas", hr);
    return false;
  }
  video_.reset();
  media_=false;
  backgroundView_ = std::move(view);
  opaqueBackdrop_.Reset();
  return true;
}
void NativeRenderer::clearBackground() {
  media_=false;
  video_.reset();backgroundView_.Reset();opaqueBackdrop_.Reset();
  backdropWidth_=backdropHeight_=0;
  lastBackdropCopy_=false;
}
ID3D11ShaderResourceView* NativeRenderer::background() const {
  if(video_ && video_->TextureView())return video_->TextureView();
  return backgroundView_.Get();
}
void NativeRenderer::setBackgroundAppearance(float brightness,float saturation,float hueDegrees,
    const std::array<std::uint8_t,3>& tint, bool solidColor) {
  brightness=std::isfinite(brightness) ? std::clamp(brightness,0.0f,4.0f) : 1.0f;
  saturation=std::isfinite(saturation) ? std::clamp(saturation,0.0f,2.5f) : 1.0f;
  float hue=std::isfinite(hueDegrees) ? std::fmod(hueDegrees,360.0f) : 0.0f;
  if(hue<0) hue+=360.0f;
  hue/=360.0f;
  const float red=tint[0]/255.0f,green=tint[1]/255.0f,blue=tint[2]/255.0f;
  const float enabled=brightness!=1.0f || saturation!=1.0f || hue!=0.0f ||
      tint[0]!=255 || tint[1]!=255 || tint[2]!=255 ? 1.0f : 0.0f;
  if(backgroundAppearance_[0]==brightness && backgroundAppearance_[1]==saturation &&
     backgroundAppearance_[2]==hue && backgroundAppearance_[3]==enabled &&
     backgroundAppearance_[4]==red && backgroundAppearance_[5]==green && backgroundAppearance_[6]==blue &&
     backgroundAppearance_[7]==static_cast<float>(solidColor)) return;
  backgroundAppearance_[0]=brightness;backgroundAppearance_[1]=saturation;
  backgroundAppearance_[2]=hue;backgroundAppearance_[3]=enabled;
  backgroundAppearance_[4]=red;backgroundAppearance_[5]=green;backgroundAppearance_[6]=blue;
  backgroundAppearance_[7]=static_cast<float>(solidColor);
  backgroundAppearanceDirty_=true;
}
bool NativeRenderer::isAnimated() const{return video_&&video_->IsAnimated();}
void NativeRenderer::setMediaPlacement(unsigned fit,float zoom,float panX,float panY) {
  mediaFit_=fit; mediaZoom_=zoom; mediaPanX_=panX; mediaPanY_=panY;
}
void NativeRenderer::setBackgroundPaused(bool paused){if(video_)video_->SetPaused(paused);}
std::string NativeRenderer::backgroundError()const{return video_?video_->LastError():std::string();}
namespace {
// Match the texture background shader's HSV adjustment and exposure shoulder.
// A uniform fill also works before a pack has ever loaded a background texture.
std::array<float,4> solidBackground(const float* appearance) {
  const float r=appearance[4],g=appearance[5],b=appearance[6];
  const float peak=std::max({r,g,b}),chroma=peak-std::min({r,g,b});
  std::array<float,4> color{r,g,b,1};
  if(chroma>0 && (appearance[2]!=0 || appearance[1]!=1)){
    float hue=peak==r?(g-b)/chroma:peak==g?(b-r)/chroma+2:(r-g)/chroma+4;
    hue=hue/6+appearance[2];
    const float saturation=std::clamp(chroma/peak*appearance[1],0.f,1.f);
    constexpr float offsets[]={0,2.f/3,1.f/3};
    for(unsigned i=0;i<3;i++){
      const float h=hue+offsets[i];
      const float pure=std::clamp(std::abs((h-std::floor(h))*6-3)-1,0.f,1.f);
      color[i]=peak*(1+saturation*(pure-1));
    }
  }
  const float gain=appearance[0];
  const float exposure=gain<=1?gain:gain/(1+(gain-1)*peak);
  for(unsigned i=0;i<3;i++)color[i]*=exposure;
  return color;
}
}
void NativeRenderer::beginFrame(bool opaqueScene) {
  if(!opaqueScene && video_)video_->UpdateFrame();
  ID3D11ShaderResourceView *nullView = nullptr;
  context_->PSSetShaderResources(0, 1, &nullView);
  ID3D11RenderTargetView *view = targetView_.Get();
  std::array<float,4> uv{1.f,1.f,0.f,0.f};
  const float aspect = video_ ? video_->AspectRatio() :
      (backdropHeight_ ? float(backdropWidth_)/backdropHeight_ : 0.f);
  if (media_ && aspect > 0.f && width_ && height_) {
    const float ratio = float(width_) / height_ / aspect;
    uv[0] = (mediaFit_ == 1 ? std::max(1.f,ratio) : std::min(1.f,ratio)) / mediaZoom_;
    uv[1] = uv[0] / ratio;
    uv[2] = (1.f-uv[0])*.5f - mediaPanX_*uv[0]*.5f;
    uv[3] = (1.f-uv[1])*.5f - mediaPanY_*uv[1]*.5f;
  }
  if (!std::equal(uv.begin(),uv.end(),backgroundAppearance_+8)) {
    std::copy(uv.begin(),uv.end(),backgroundAppearance_+8);
    backgroundAppearanceDirty_=true;
  }
  const bool solidBlack = backgroundAppearance_[0] == 0.0f ||
      (backgroundAppearance_[4] == 0.0f && backgroundAppearance_[5] == 0.0f && backgroundAppearance_[6] == 0.0f);
  const bool direct=!opaqueScene && !solidBlack && backgroundAppearance_[7]==0.f && allowBackdropCopy_ && backgroundAppearance_[3]==0.0f && !video_ && opaqueBackdrop_ &&
                    backdropWidth_==width_ && backdropHeight_==height_ && uv[0]==1 && uv[1]==1 && uv[2]==0 && uv[3]==0;
  if (direct) {
    context_->OMSetRenderTargets(0,nullptr,nullptr);
    context_->CopyResource(target_.Get(),opaqueBackdrop_.Get());
  }
  lastBackdropCopy_=direct;
  context_->OMSetRenderTargets(1,&view,nullptr);
  D3D11_VIEWPORT viewport{};
  viewport.Width=static_cast<float>(width_);
  viewport.Height=static_cast<float>(height_);
  viewport.MaxDepth=1;
  context_->RSSetViewports(1,&viewport);
  if (direct || opaqueScene) return;
  // The background shader yields opaque black independently of the source.
  // Preserve the source texture for effects which sample it.
  if (backgroundAppearance_[7]!=0.f) {
    const auto fill=solidBackground(backgroundAppearance_);
    context_->ClearRenderTargetView(view,fill.data());
    return;
  }
  if (solidBlack || !background()) {
    const float black[4]={0,0,0,1};
    context_->ClearRenderTargetView(view,black);
    return;
  }
  // The opaque full-screen shader overwrites all pixels; no pre-clear.
  context_->GSSetShader(nullptr, nullptr, 0);
  context_->HSSetShader(nullptr, nullptr, 0);
  context_->DSSetShader(nullptr, nullptr, 0);
  context_->RSSetState(rasterizer_.Get());
  context_->OMSetBlendState(nullptr, nullptr, 0xffffffff);
  context_->OMSetDepthStencilState(nullptr, 0);
  context_->IASetInputLayout(nullptr);
  context_->IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
  context_->VSSetShader(vertexShader_.Get(), nullptr, 0);
  context_->PSSetShader(pixelShader_.Get(), nullptr, 0);
  if(backgroundAppearanceDirty_) {
    context_->UpdateSubresource(backgroundConstants_.Get(),0,nullptr,backgroundAppearance_,0,0);
    backgroundAppearanceDirty_=false;
  }
  auto* constants=backgroundConstants_.Get();
  context_->PSSetConstantBuffers(0,1,&constants);
  ID3D11ShaderResourceView *source = background();
  ID3D11SamplerState *sampler = sampler_.Get();
  context_->PSSetShaderResources(0, 1, &source);
  context_->PSSetSamplers(0, 1, &sampler);
  context_->Draw(3, 0);
  context_->PSSetShaderResources(0, 1, &nullView);
  constants=nullptr;
  context_->PSSetConstantBuffers(0,1,&constants);
}
bool NativeRenderer::validateFrame(const std::function<bool()> &draw, std::string &error) {
  if (!targetView_) {
    error = "Graphics output is unavailable";
    return false;
  }
  if (video_ && !video_->TextureView()) {
    const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(2);
    do {
      if (video_->UpdateFrame())
        break;
      if (const auto issue = video_->LastError(); !issue.empty()) {
        error = issue;
        return false;
      }
      if (deviceLost()) {
        error = "Graphics device was lost while decoding the first video frame";
        return false;
      }
      Sleep(1);
    } while (std::chrono::steady_clock::now() < deadline);
    if (!video_->TextureView()) {
      error = "Video did not decode its first frame within two seconds";
      return false;
    }
  }
  D3D11_TEXTURE2D_DESC desc{};
  target_->GetDesc(&desc);
  desc.BindFlags = D3D11_BIND_RENDER_TARGET;
  desc.MiscFlags = 0;
  ComPtr<ID3D11Texture2D> target;
  ComPtr<ID3D11RenderTargetView> view;
  ComPtr<ID3D11Query> completion;
  const D3D11_QUERY_DESC query{D3D11_QUERY_EVENT, 0};
  HRESULT hr = device_->CreateTexture2D(&desc, nullptr, &target);
  if (SUCCEEDED(hr))
    hr = device_->CreateRenderTargetView(target.Get(), nullptr, &view);
  if (SUCCEEDED(hr))
    hr = device_->CreateQuery(&query, &completion);
  if (FAILED(hr)) {
    error = hrError("Create first-frame validation resources", hr);
    return false;
  }

  target_.Swap(target);
  targetView_.Swap(view);
  const bool previousBackdropCopy = lastBackdropCopy_;
  const auto restoreTarget = [&] {
    context_->ClearState();
    target_.Swap(target);
    targetView_.Swap(view);
    lastBackdropCopy_ = previousBackdropCopy;
  };
  bool drawn;
  try {
    drawn = draw();
  } catch (...) {
    restoreTarget();
    throw;
  }
  restoreTarget();
  if (!drawn) {
    if (error.empty())
      error = "Could not draw the first wallpaper frame";
    return false;
  }

  context_->End(completion.Get());
  context_->Flush();
  const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(1);
  do {
    hr = context_->GetData(completion.Get(), nullptr, 0, D3D11_ASYNC_GETDATA_DONOTFLUSH);
    if (const auto removed = device_->GetDeviceRemovedReason(); FAILED(removed))
      hr = removed;
    if (hr == S_OK)
      return true;
    if (FAILED(hr)) {
      error = hrError("Complete first wallpaper frame", hr);
      return false;
    }
    Sleep(1);
  } while (std::chrono::steady_clock::now() < deadline);
  error = "GPU did not complete the first wallpaper frame within one second";
  return false;
}

bool NativeRenderer::deviceLost() const noexcept {
  return device_ && (lastPresentResult_ == DXGI_ERROR_DEVICE_REMOVED ||
                     lastPresentResult_ == DXGI_ERROR_DEVICE_RESET ||
                     FAILED(device_->GetDeviceRemovedReason()));
}

bool NativeRenderer::present(std::string &error) {
  if (!swapChain_)
    return true;
  // A busy GPU or a foreground game's display-mode transition must not block
  // the same thread that receives Raw Input and local control messages.
  HRESULT hr = swapChain_->Present(0, DXGI_PRESENT_DO_NOT_WAIT);
  lastPresentResult_ = hr;
  presentPending_ = hr == DXGI_ERROR_WAS_STILL_DRAWING;
  if (presentPending_) {
    ++presentsBusy_;
    return true;
  }
  if (FAILED(hr)) {
    error = hrError("Present", hr);
    return false;
  }
  ++presentsCompleted_;
  return true;
}
bool NativeRenderer::saveFrame(const std::filesystem::path &file,
                               std::string &error) {
  D3D11_TEXTURE2D_DESC desc{};
  target_->GetDesc(&desc);
  desc.Usage = D3D11_USAGE_STAGING;
  desc.BindFlags = 0;
  desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
  desc.MiscFlags = 0;
  ComPtr<ID3D11Texture2D> readback;
  HRESULT hr = device_->CreateTexture2D(&desc, nullptr, &readback);
  if (FAILED(hr)) {
    error = hrError("Create readback", hr);
    return false;
  }
  context_->CopyResource(readback.Get(), target_.Get());
  D3D11_MAPPED_SUBRESOURCE mapped{};
  hr = context_->Map(readback.Get(), 0, D3D11_MAP_READ, 0, &mapped);
  if (FAILED(hr)) {
    error = hrError("Read back render target", hr);
    return false;
  }
  const bool saved=saveRgbaFrame(file,width_,height_,mapped.RowPitch,static_cast<BYTE*>(mapped.pData),error);
  context_->Unmap(readback.Get(),0);
  return saved;
}
bool saveRgbaFrame(const std::filesystem::path &file,unsigned width,unsigned height,UINT rowPitch,BYTE *pixels,std::string &error) {
  ComPtr<IWICImagingFactory> factory;
  ComPtr<IWICStream> stream;
  ComPtr<IWICBitmapEncoder> encoder;
  ComPtr<IWICBitmapFrameEncode> frame;
  HRESULT hr = CoCreateInstance(CLSID_WICImagingFactory, nullptr, CLSCTX_INPROC_SERVER,
                        IID_PPV_ARGS(&factory));
  if (SUCCEEDED(hr))
    hr = factory->CreateStream(&stream);
  if (SUCCEEDED(hr))
    hr = stream->InitializeFromFilename(file.c_str(), GENERIC_WRITE);
  if (SUCCEEDED(hr))
    hr = factory->CreateEncoder(GUID_ContainerFormatPng, nullptr, &encoder);
  if (SUCCEEDED(hr))
    hr = encoder->Initialize(stream.Get(), WICBitmapEncoderNoCache);
  if (SUCCEEDED(hr))
    hr = encoder->CreateNewFrame(&frame, nullptr);
  if (SUCCEEDED(hr))
    hr = frame->Initialize(nullptr);
  if (SUCCEEDED(hr))
    hr = frame->SetSize(width, height);
  WICPixelFormatGUID format = GUID_WICPixelFormat32bppBGRA;
  if (SUCCEEDED(hr))
    hr = frame->SetPixelFormat(&format);
  ComPtr<IWICBitmap> source;
  ComPtr<IWICFormatConverter> conversion;
  if (SUCCEEDED(hr))
    hr = factory->CreateBitmapFromMemory(width,height,GUID_WICPixelFormat32bppRGBA,
          rowPitch,rowPitch*height,pixels,&source);
  if (SUCCEEDED(hr))
    hr = factory->CreateFormatConverter(&conversion);
  if (SUCCEEDED(hr))
    hr = conversion->Initialize(source.Get(),format,WICBitmapDitherTypeNone,nullptr,0,WICBitmapPaletteTypeCustom);
  if (SUCCEEDED(hr))
    hr = frame->WriteSource(conversion.Get(),nullptr);
  if (SUCCEEDED(hr))
    hr = frame->Commit();
  if (SUCCEEDED(hr))
    hr = encoder->Commit();
  if (FAILED(hr)) {
    error = hrError("Save PNG", hr);
    return false;
  }
  return true;
}
} // namespace action75
