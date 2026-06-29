//! Native-Windows DX12 present-hook validation harness.
//!
//! Why this exists: the in-game overlay (hudhook DX12 present-hook + Dear ImGui, `coop/overlay.rs`)
//! renders fine on our Linux rig (vkd3d/Proton) but *crashes on native Windows NVIDIA* at the first
//! hooked Present (first friend test, RTX 3080 — full anatomy in `docs/OVERLAY-RENDERING.md` >
//! "Native-Windows Crash"). We were debugging that blind, with no Windows box. This harness is the
//! Windows box stand-in: a minimal D3D12 app that presents real frames and then injects the SAME
//! machinery the overlay uses — hudhook's `ImguiDx12Hooks` + the identical imgui font bake — into its
//! own live swapchain, mid-flight, from a side thread (exactly how the game does it). So the crashing
//! path runs in a plain Windows VM (WARP / virtio-gpu) or on real hardware, with no ELDEN RING.
//!
//! What it faithfully reproduces (covers crash hypotheses #1 and #3 from the doc): hudhook's MinHook
//! detour applied to an *already-presenting* swapchain vtable, off-thread; and imgui's DX12 backend
//! baking + uploading the overlay's real font atlas on the GPU.
//!
//! What it does NOT reproduce (deferred to the friend's real machine, the single super-validated gate):
//! ELDEN RING's exact swapchain flags (mirror via env if a rig probe pins them), the DLSS interposer
//! (hypothesis #2), and any NVIDIA-driver-specific present threading (no NVIDIA in a VM).
//!
//! The log mirrors the overlay's breadcrumb lines and forces a per-record flush, so a crash can't eat
//! the decisive tail line — diff it against the rig baseline in `docs/OVERLAY-RENDERING.md`.
//!
//! Env knobs (all optional):
//!   DX12_HARNESS_LOG       log file path                       (default: dx12-harness.log)
//!   DX12_HARNESS_WARMUP    frames to present before hooking     (default: 120 — "title screen presenting")
//!   DX12_HARNESS_FRAMES    total frames then exit, 0 = run till close (default: 0)
//!   DX12_HARNESS_BUFFERS   swapchain buffer count               (default: 3 — flip-discard, AAA-typical)
//!   DX12_HARNESS_VSYNC     present sync interval 0/1             (default: 1)
//!   DX12_HARNESS_WARP      1 = force the WARP software adapter   (default: 0 — default adapter)
//!   DX12_HARNESS_HOOK_THREAD 1 = install hook off-thread         (default: 1 — mirrors the game)
//!   DX12_HARNESS_NO_HOOK   1 = never install the hook (control)  (default: 0)

use std::io::{self, Write};
use std::sync::atomic::{AtomicU64, Ordering};

use hudhook::hooks::dx12::ImguiDx12Hooks;
use hudhook::imgui::{Context, FontConfig, FontSource, Ui};
use hudhook::{Hudhook, ImguiRenderLoop, RenderContext};

use windows::core::{w, Interface, Result, HRESULT};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WAIT_OBJECT_0, WPARAM};
use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Graphics::Gdi::HBRUSH;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::Win32::UI::WindowsAndMessaging::*;

// The overlay's real menu font, baked identically below — so the imgui DX12 font-upload path (crash
// hypothesis #3) runs on the exact same glyph data the game ships.
const MENU_FONT: &[u8] = include_bytes!("../../unseamless-coop/assets/menu-font.otf");
const MENU_FONT_SIZE: f32 = 16.0;

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;

// ---- config ------------------------------------------------------------------------------------

struct Config {
    log_path: String,
    warmup: u64,
    frames: u64,
    buffers: u32,
    vsync: u32,
    warp: bool,
    hook_thread: bool,
    hook: bool,
}

impl Config {
    fn from_env() -> Self {
        let env_u64 = |k: &str, d: u64| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
        let env_bool = |k: &str, d: bool| std::env::var(k).ok().map(|v| v == "1" || v == "true").unwrap_or(d);
        Config {
            log_path: std::env::var("DX12_HARNESS_LOG").unwrap_or_else(|_| "dx12-harness.log".into()),
            warmup: env_u64("DX12_HARNESS_WARMUP", 120),
            frames: env_u64("DX12_HARNESS_FRAMES", 0),
            buffers: env_u64("DX12_HARNESS_BUFFERS", 3) as u32,
            vsync: env_u64("DX12_HARNESS_VSYNC", 1) as u32,
            warp: env_bool("DX12_HARNESS_WARP", false),
            hook_thread: env_bool("DX12_HARNESS_HOOK_THREAD", true),
            hook: !env_bool("DX12_HARNESS_NO_HOOK", false),
        }
    }
}

// ---- logging (per-record flush, so a crash keeps the tail line) ---------------------------------

/// Wraps the log file so every record is flushed to disk immediately. The native crash kills the
/// process at the first hooked Present; without this the decisive last line is lost in the buffer
/// (3 of 4 friend logs lost their tail this way — see the doc).
struct FlushWriter(std::fs::File);
impl Write for FlushWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.0.write(buf)?;
        self.0.flush()?;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

fn init_logging(path: &str) {
    use simplelog::{CombinedLogger, Config as LogConfig, LevelFilter, SimpleLogger, WriteLogger};
    let file = std::fs::File::create(path).expect("create harness log file");
    // Trace level so hudhook's own dx12.rs breadcrumbs (`Call IDXGISwapChain::Present trampoline`,
    // `ExecuteCommandLists invoked`, `Found command queue pointer ...`) reach the log — hudhook's
    // `tracing` events forward to the `log` crate because we install no tracing subscriber.
    // `SimpleLogger` (no-color, always available — the colored `TermLogger` needs simplelog's
    // `termcolor` feature, which is off here, matching the cdylib) mirrors to stderr for live runs.
    let _ = CombinedLogger::init(vec![
        SimpleLogger::new(LevelFilter::Trace, LogConfig::default()),
        WriteLogger::new(LevelFilter::Trace, LogConfig::default(), FlushWriter(file)),
    ]);
}

// ---- the injected render loop (mirrors coop/overlay.rs's initialize/render) ----------------------

static RENDER_FRAMES: AtomicU64 = AtomicU64::new(0);

struct TestLoop {
    logged_first_render: bool,
}

impl TestLoop {
    fn new() -> Self {
        TestLoop { logged_first_render: false }
    }
}

impl ImguiRenderLoop for TestLoop {
    fn initialize<'a>(&'a mut self, ctx: &mut Context, _rc: &'a mut dyn RenderContext) {
        // Same breadcrumb + same font bake as coop/overlay.rs::initialize, so the GPU font-upload path
        // (crash hypothesis #3) is exercised on identical data. If a crash log shows this line but not
        // `first render frame reached`, the fault is in hudhook's context completion, not our render.
        log::info!("overlay: hudhook initialize() reached (baking fonts)");
        let fonts = ctx.fonts();
        fonts.add_font(&[FontSource::DefaultFontData { config: None }]);
        fonts.add_font(&[FontSource::TtfData {
            data: MENU_FONT,
            size_pixels: MENU_FONT_SIZE,
            config: Some(FontConfig { oversample_h: 1, oversample_v: 1, pixel_snap_h: true, ..FontConfig::default() }),
        }]);
    }

    fn render(&mut self, ui: &mut Ui) {
        if !self.logged_first_render {
            self.logged_first_render = true;
            log::info!("overlay: first render frame reached (render_inner)");
        }
        let n = RENDER_FRAMES.fetch_add(1, Ordering::Relaxed) + 1;
        ui.window("dx12-harness")
            .size([360.0, 90.0], hudhook::imgui::Condition::FirstUseEver)
            .build(|| {
                ui.text("unseamless-coop DX12 present-hook harness");
                ui.text(format!("rendered frames: {n}"));
            });
    }
}

// ---- D3D12 plumbing -----------------------------------------------------------------------------

/// Minimal D3D12 state: enough to clear the backbuffer and Present every frame. hudhook needs the
/// per-frame `ExecuteCommandLists` (to find the command queue) + `Present` (to draw), so a clear-only
/// loop is sufficient to drive the whole hook path.
struct Renderer {
    // Held to keep the D3D12 device alive for the renderer's lifetime (its child objects reference it);
    // not read directly after construction.
    #[allow(dead_code)]
    device: ID3D12Device,
    queue: ID3D12CommandQueue,
    swapchain: IDXGISwapChain3,
    rtv_heap: ID3D12DescriptorHeap,
    rtv_size: usize,
    targets: Vec<ID3D12Resource>,
    allocator: ID3D12CommandAllocator,
    list: ID3D12GraphicsCommandList,
    fence: ID3D12Fence,
    fence_val: u64,
    fence_event: windows::Win32::Foundation::HANDLE,
}

impl Renderer {
    fn new(hwnd: HWND, cfg: &Config) -> Result<Self> {
        unsafe {
            let factory: IDXGIFactory4 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0))?;

            // Pick the adapter. WARP (Microsoft's software D3D12) is the most likely path in a plain
            // VM and is forceable for a deterministic, GPU-independent run; otherwise use the default
            // adapter (real GPU on a passthrough/native box).
            let mut device: Option<ID3D12Device> = None;
            if cfg.warp {
                let warp: IDXGIAdapter = factory.EnumWarpAdapter()?;
                D3D12CreateDevice(&warp, D3D_FEATURE_LEVEL_11_0, &mut device)?;
                log::info!("d3d12: using WARP software adapter (forced)");
            } else {
                D3D12CreateDevice(None, D3D_FEATURE_LEVEL_11_0, &mut device)?;
                log::info!("d3d12: using default adapter");
            }
            let device = device.expect("D3D12CreateDevice returned no device");

            let qdesc = D3D12_COMMAND_QUEUE_DESC { Type: D3D12_COMMAND_LIST_TYPE_DIRECT, ..Default::default() };
            let queue: ID3D12CommandQueue = device.CreateCommandQueue(&qdesc)?;

            let scdesc = DXGI_SWAP_CHAIN_DESC1 {
                BufferCount: cfg.buffers,
                Width: WIDTH,
                Height: HEIGHT,
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                ..Default::default()
            };
            let sc1 = factory.CreateSwapChainForHwnd(&queue, hwnd, &scdesc, None, None)?;
            let swapchain: IDXGISwapChain3 = sc1.cast()?;
            log::info!(
                "d3d12: swapchain up ({}x{}, {} buffers, FLIP_DISCARD, R8G8B8A8)",
                WIDTH, HEIGHT, cfg.buffers
            );

            let heapdesc = D3D12_DESCRIPTOR_HEAP_DESC {
                Type: D3D12_DESCRIPTOR_HEAP_TYPE_RTV,
                NumDescriptors: cfg.buffers,
                ..Default::default()
            };
            let rtv_heap: ID3D12DescriptorHeap = device.CreateDescriptorHeap(&heapdesc)?;
            let rtv_size = device.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_RTV) as usize;

            let mut targets = Vec::with_capacity(cfg.buffers as usize);
            let mut handle = rtv_heap.GetCPUDescriptorHandleForHeapStart();
            for i in 0..cfg.buffers {
                let res: ID3D12Resource = swapchain.GetBuffer(i)?;
                device.CreateRenderTargetView(&res, None, handle);
                targets.push(res);
                handle.ptr += rtv_size;
            }

            let allocator: ID3D12CommandAllocator = device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)?;
            let list: ID3D12GraphicsCommandList =
                device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None)?;
            list.Close()?;

            let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE)?;
            let fence_event = CreateEventW(None, false, false, None)?;

            Ok(Renderer {
                device,
                queue,
                swapchain,
                rtv_heap,
                rtv_size,
                targets,
                allocator,
                list,
                fence,
                fence_val: 0,
                fence_event,
            })
        }
    }

    fn rtv_handle(&self, index: u32) -> D3D12_CPU_DESCRIPTOR_HANDLE {
        let mut h = unsafe { self.rtv_heap.GetCPUDescriptorHandleForHeapStart() };
        h.ptr += self.rtv_size * index as usize;
        h
    }

    /// Clear the current backbuffer to a slowly cycling colour and Present. Serializes the GPU with a
    /// fence each frame — fine for a test, and keeps the resource state simple.
    fn render_frame(&mut self, frame: u64, vsync: u32) -> Result<()> {
        unsafe {
            let index = self.swapchain.GetCurrentBackBufferIndex();
            self.allocator.Reset()?;
            self.list.Reset(&self.allocator, None)?;

            self.list.ResourceBarrier(&[transition(
                &self.targets[index as usize],
                D3D12_RESOURCE_STATE_PRESENT,
                D3D12_RESOURCE_STATE_RENDER_TARGET,
            )]);

            let rtv = self.rtv_handle(index);
            self.list.OMSetRenderTargets(1, Some(&rtv), false, None);
            let t = (frame as f32 * 0.01).sin().abs();
            self.list.ClearRenderTargetView(rtv, &[0.1, 0.1 + 0.3 * t, 0.3, 1.0], None);

            self.list.ResourceBarrier(&[transition(
                &self.targets[index as usize],
                D3D12_RESOURCE_STATE_RENDER_TARGET,
                D3D12_RESOURCE_STATE_PRESENT,
            )]);

            self.list.Close()?;
            let cl: ID3D12CommandList = self.list.cast()?;
            self.queue.ExecuteCommandLists(&[Some(cl)]);

            self.swapchain.Present(vsync, DXGI_PRESENT(0)).ok()?;

            // Wait for the frame so the next Reset is safe. BOUNDED, not INFINITE: a GPU/driver stall
            // is exactly the failure class this harness hunts, and an unbounded wait would hang the
            // process forever — holding the swapchain + blocking every later `win.sh run`. A trivial
            // 720p clear completes in well under a second even on WARP, so a 5s ceiling is pure
            // headroom; on timeout we surface it and abort the run instead of zombie-ing.
            self.fence_val += 1;
            self.queue.Signal(&self.fence, self.fence_val)?;
            if self.fence.GetCompletedValue() < self.fence_val {
                self.fence.SetEventOnCompletion(self.fence_val, self.fence_event)?;
                let waited = WaitForSingleObject(self.fence_event, 5000);
                if waited != WAIT_OBJECT_0 {
                    log::error!("d3d12: fence wait did not complete ({waited:?}) at frame {frame} — GPU stalled?; aborting");
                    return Err(HRESULT(0x8000_4005u32 as i32).into()); // E_FAIL
                }
            }
        }
        Ok(())
    }
}

/// A present→render-target (or reverse) transition barrier. `pResource` is filled via `transmute_copy`
/// without an AddRef and wrapped in `ManuallyDrop`, so the barrier never releases the resource — the
/// standard windows-rs D3D12 pattern.
fn transition(
    resource: &ID3D12Resource,
    before: D3D12_RESOURCE_STATES,
    after: D3D12_RESOURCE_STATES,
) -> D3D12_RESOURCE_BARRIER {
    D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 {
            Transition: std::mem::ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                pResource: unsafe { std::mem::transmute_copy(resource) },
                Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                StateBefore: before,
                StateAfter: after,
            }),
        },
    }
}

// ---- window -------------------------------------------------------------------------------------

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

fn create_window(hinstance: HINSTANCE) -> Result<HWND> {
    unsafe {
        let class = w!("dx12_harness_window");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hbrBackground: HBRUSH::default(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            w!("unseamless-coop dx12-harness"),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            WIDTH as i32,
            HEIGHT as i32,
            None,
            None,
            Some(hinstance),
            None,
        )?;
        let _ = ShowWindow(hwnd, SW_SHOW);
        Ok(hwnd)
    }
}

/// Install the SAME hook the overlay installs (`coop/overlay.rs::install`): hudhook's DX12 present
/// hook with our `TestLoop`. hudhook patches the shared swapchain vtable, so it takes effect on our
/// already-presenting swapchain at the next Present — the exact mid-flight injection the game does.
fn install_hook(module: usize) {
    let hmodule = hudhook::windows::Win32::Foundation::HINSTANCE(module as *mut core::ffi::c_void);
    match Hudhook::builder()
        .with::<ImguiDx12Hooks>(TestLoop::new())
        .with_hmodule(hmodule)
        .build()
        .apply()
    {
        Ok(()) => log::info!("overlay: DX12 present-hook installed; waiting for the swapchain"),
        Err(e) => log::error!("overlay: hook install failed ({e:?}); no overlay this session"),
    }
}

fn main() -> Result<()> {
    let cfg = Config::from_env();
    init_logging(&cfg.log_path);
    log::info!(
        "dx12-harness start: build {} | warmup={} frames={} buffers={} vsync={} warp={} hook={} hook_thread={}",
        option_env!("UNSEAMLESS_BUILD_ID").unwrap_or("nogit"),
        cfg.warmup,
        cfg.frames,
        cfg.buffers,
        cfg.vsync,
        cfg.warp,
        cfg.hook,
        cfg.hook_thread,
    );

    let hinstance: HINSTANCE = unsafe { GetModuleHandleW(None)?.into() };
    let module = hinstance.0 as usize; // Send-safe handle for the off-thread hook install
    let hwnd = create_window(hinstance)?;
    let mut renderer = Renderer::new(hwnd, &cfg)?;

    let mut frame: u64 = 0;
    let mut hooked = false;
    let mut quit = false;

    while !quit {
        // Pump messages without blocking, so the present loop keeps the swapchain alive.
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    quit = true;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        if quit {
            break;
        }

        if let Err(e) = renderer.render_frame(frame, cfg.vsync) {
            log::error!("d3d12: render_frame failed at frame {frame}: {e:?}");
            break;
        }
        frame += 1;

        // After the warmup, inject the hook into the live swapchain — off-thread by default, mirroring
        // the game (overlay install runs on its own short-lived thread, not the present thread).
        if cfg.hook && !hooked && frame >= cfg.warmup {
            hooked = true;
            log::info!("dx12-harness: warmup complete ({frame} frames presented); injecting overlay hook");
            if cfg.hook_thread {
                std::thread::spawn(move || install_hook(module));
            } else {
                install_hook(module);
            }
        }

        if cfg.frames != 0 && frame >= cfg.frames {
            log::info!("dx12-harness: reached frame cap ({}); exiting", cfg.frames);
            break;
        }
    }

    log::info!(
        "dx12-harness done: presented {} frames, overlay rendered {} frames",
        frame,
        RENDER_FRAMES.load(Ordering::Relaxed)
    );
    Ok(())
}
