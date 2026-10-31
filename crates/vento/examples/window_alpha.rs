//! Shows a frameless window with rounded, soft-edged shapes on a
//! transparent background, drawn the way vento draws its own: winit and
//! Slint's software renderer. For checking transparency by eye or with a
//! screenshot.
//!
//! `cargo run -p vento --example window_alpha [-- --overlay] [-- --secs N]`
//! `--overlay` hands the window to `Platform::make_non_key` first, as the
//! toasts are; the window closes itself after `N` seconds (default 6).

use std::time::Duration;

use raw_window_handle::HasWindowHandle;

slint::slint! {
    export component Probe inherits Window {
        no-frame: true;
        background: transparent;
        width: 360px;
        height: 200px;
        // Half-transparent over whatever is behind the window.
        Rectangle {
            x: 0px;
            y: 186px;
            width: 360px;
            height: 14px;
            background: #0050ff80;
        }
        Rectangle {
            x: 20px;
            y: 20px;
            width: 320px;
            height: 160px;
            border-radius: 28px;
            background: #ff6a00;
            drop-shadow-blur: 12px;
            drop-shadow-color: #00000080;
            Rectangle {
                x: 24px;
                y: 40px;
                width: 80px;
                height: 80px;
                border-radius: 40px;
                background: #ffffff80;
            }
            Text {
                x: 120px;
                text: "Vento";
                font-size: 40px;
                color: white;
            }
        }
    }
}

fn main() -> Result<(), slint::PlatformError> {
    env_logger::init();
    let args: Vec<String> = std::env::args().collect();
    let overlay = args.iter().any(|a| a == "--overlay");
    let secs = args
        .iter()
        .position(|a| a == "--secs")
        .and_then(|i| args.get(i + 1)?.parse().ok())
        .unwrap_or(6);
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("software".into())
        .select()?;
    let probe = Probe::new()?;
    probe
        .window()
        .set_position(slint::LogicalPosition::new(200.0, 200.0));
    probe.show()?;
    if overlay {
        let platform = vento_platform::current();
        let handle = probe.window().window_handle();
        match handle.window_handle() {
            Ok(h) => {
                if let Err(e) = platform.make_non_key(h.as_raw()) {
                    eprintln!("make_non_key: {e}");
                }
            }
            Err(e) => eprintln!("no window handle: {e}"),
        }
    }
    slint::Timer::single_shot(Duration::from_secs(secs), || {
        slint::quit_event_loop().ok();
    });
    slint::run_event_loop_until_quit()
}
