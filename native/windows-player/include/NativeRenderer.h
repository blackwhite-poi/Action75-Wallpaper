#pragma once
#include <array>
#include <cstdint>
#include <d3d11.h>
#include <dxgi1_3.h>
#include <filesystem>
#include <functional>
#include <memory>
#include <string>
#include <windows.h>
#include <wrl/client.h>

namespace action75 {
bool saveRgbaFrame(const std::filesystem::path &file,unsigned width,unsigned height,UINT rowPitch,BYTE *pixels,std::string &error);
class VideoBackground;
enum class GpuPreference { Discrete, Integrated };

class NativeRenderer {
public:
  struct BackgroundSnapshot {
    Microsoft::WRL::ComPtr<ID3D11ShaderResourceView> view;
    Microsoft::WRL::ComPtr<ID3D11Texture2D> opaque;
    std::unique_ptr<VideoBackground> video;
    unsigned width=0,height=0;
    bool copied=false;
    std::array<float, 12> appearance{};
    bool media=false;
    unsigned fit=0;
    float zoom=1,panX=0,panY=0;
    BackgroundSnapshot();
    ~BackgroundSnapshot();
    BackgroundSnapshot(BackgroundSnapshot&&) noexcept;
    BackgroundSnapshot& operator=(BackgroundSnapshot&&) noexcept;
  };
  NativeRenderer();
  // Native wallpapers use the worker's device and receive a target on each render callback.
  bool initializeForPlugin(ID3D11Device *device, ID3D11DeviceContext *context,
                           unsigned width, unsigned height, std::string &error);
  bool setPluginTarget(ID3D11RenderTargetView *target);
  bool initialize(HWND window, unsigned width, unsigned height, bool offscreen,
                  std::string &error, bool hardwareOffscreen = false,
                  GpuPreference preference = GpuPreference::Discrete, const LUID *adapter = nullptr);
  void releaseResources() noexcept;
  bool resize(unsigned width, unsigned height, std::string &error);
  bool loadBackground(const std::filesystem::path &file, std::string &error,
                      unsigned maxPixels = 33554432u, bool media = false);
  // Bake a static Effekseer layer once at the pack's 1920x720 reference size.
  // Failure leaves the previous image, video or cached canvas intact.
  bool loadCanvasBackground(const std::filesystem::path &file,
                            std::string &error);
  void clearBackground();
  BackgroundSnapshot takeBackground() noexcept;
  void restoreBackground(BackgroundSnapshot&& snapshot) noexcept;
  void beginFrame(bool opaqueScene = false);
  // Draw on this device at the current output size and wait for GPU completion.
  // The display buffer is left intact even if preparation fails.
  bool validateFrame(const std::function<bool()> &draw, std::string &error);
  bool usingBackdropCopy() const noexcept { return lastBackdropCopy_; }
  void allowBackdropCopy(bool allow) noexcept { allowBackdropCopy_=allow; }
  bool present(std::string &error);
  bool presentPending() const noexcept { return presentPending_; }
  std::uint64_t presentsCompleted() const noexcept { return presentsCompleted_; }
  std::uint64_t presentsBusy() const noexcept { return presentsBusy_; }
  HRESULT lastPresentResult() const noexcept { return lastPresentResult_; }
  bool deviceLost() const noexcept;
  HANDLE deviceRemovedEvent() const noexcept { return deviceRemovedEvent_; }
  bool saveFrame(const std::filesystem::path &file, std::string &error);
  ID3D11Device *device() const { return device_.Get(); }
  uint64_t adapterLuid() const noexcept { return adapterLuid_; }
  const std::wstring &adapterName() const noexcept { return adapterName_; }
  ID3D11DeviceContext *context() const { return context_.Get(); }
  ID3D11ShaderResourceView *background() const;
  // Adjust only the background draw, in its visible sRGB code domain. The
  // source SRV returned by background() remains unchanged for effect sampling.
  // Neutral (1,1,0) retains the original pixels and opaque CopyResource path.
  void setBackgroundAppearance(float brightness,float saturation,float hueDegrees,
      const std::array<std::uint8_t,3>& tint={255,255,255}, bool solidColor=false);
  void setMediaPlacement(unsigned fit, float zoom, float panX, float panY);
  bool isAnimated() const;
  void setBackgroundPaused(bool paused);
  std::string backgroundError() const;
  HANDLE frameReady() const { return frameReady_; }
  ID3D11Texture2D *outputTexture() const { return target_.Get(); }
  bool hasRenderTarget() const { return targetView_.Get() != nullptr; }
  unsigned width() const { return width_; }
  unsigned height() const { return height_; }
  ~NativeRenderer();

private:
  bool initializeBackgroundPipeline(std::string &error);
  bool initializeResources(HWND window, unsigned width, unsigned height, bool offscreen,
                           std::string &error, bool hardwareOffscreen, GpuPreference preference, const LUID *requiredAdapter);
  using ComDevice = Microsoft::WRL::ComPtr<ID3D11Device>;
  ComDevice device_;
  uint64_t adapterLuid_ = 0;
  std::wstring adapterName_;
  Microsoft::WRL::ComPtr<ID3D11DeviceContext> context_;
  Microsoft::WRL::ComPtr<IDXGISwapChain1> swapChain_;
  Microsoft::WRL::ComPtr<ID3D11Texture2D> target_;
  Microsoft::WRL::ComPtr<ID3D11RenderTargetView> targetView_;
  Microsoft::WRL::ComPtr<ID3D11ShaderResourceView> backgroundView_;
  Microsoft::WRL::ComPtr<ID3D11Texture2D> opaqueBackdrop_;
  unsigned backdropWidth_=0, backdropHeight_=0;
  bool allowBackdropCopy_=true, lastBackdropCopy_=false;
  Microsoft::WRL::ComPtr<ID3D11VertexShader> vertexShader_;
  Microsoft::WRL::ComPtr<ID3D11PixelShader> pixelShader_;
  Microsoft::WRL::ComPtr<ID3D11Buffer> backgroundConstants_;
  // brightness, saturation, hue in turns, grade-enabled; RGB tint and solid-background flag.
  // The third vector maps screen UV to media UV (scale.xy, offset.zw).
  float backgroundAppearance_[12]={1,1,0,0,1,1,1,0,1,1,0,0};
  bool media_ = false;
  unsigned mediaFit_ = 0;
  float mediaZoom_ = 1.f, mediaPanX_ = 0.f, mediaPanY_ = 0.f;
  bool backgroundAppearanceDirty_=true;
  Microsoft::WRL::ComPtr<ID3D11SamplerState> sampler_;
  Microsoft::WRL::ComPtr<ID3D11RasterizerState> rasterizer_;
  unsigned width_ = 0, height_ = 0;
  HANDLE frameReady_ = nullptr;
  HANDLE deviceRemovedEvent_ = nullptr;
  DWORD deviceRemovedRegistration_ = 0;
  bool offscreen_ = false;
  bool presentPending_ = false;
  std::uint64_t presentsCompleted_ = 0, presentsBusy_ = 0;
  HRESULT lastPresentResult_ = S_OK;
  std::unique_ptr<VideoBackground> video_;
};
} // namespace action75
