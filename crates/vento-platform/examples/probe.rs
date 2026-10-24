//! Prints the screens, the Pocket island home and the probe binary's Finder
//! icon size, then platform input changes for 15 s (or the seconds given)
//! so the sampler can be checked by hand: drag a Finder file, press Ctrl,
//! press Esc, release. A new drag also prints its files.

use std::time::{Duration, Instant};

use vento_core::trigger::InputSample;

fn changed(a: &InputSample, b: &InputSample) -> bool {
    a.left_down != b.left_down
        || a.modifiers != b.modifiers
        || a.escape_down != b.escape_down
        || a.drag_change_count != b.drag_change_count
        || a.drag_has_files != b.drag_has_files
}

fn main() {
    env_logger::init();
    let platform = vento_platform::current();
    platform.prepare_app();
    println!("screens: {:?}", platform.screens());
    println!("pocket_home: {:?}", platform.pocket_home());
    let icon = std::env::current_exe()
        .ok()
        .and_then(|exe| platform.file_icon(&exe, 32));
    match icon {
        Some(i) => {
            let visible = i
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|p| p[3] > 0)
                .count();
            println!("file_icon: {}x{} ({visible} visible px)", i.width, i.height);
        }
        None => println!("file_icon: None"),
    }

    let mut last = platform.sample_input();
    println!("{last:?}");
    let secs = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(15);
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        std::thread::sleep(Duration::from_millis(8));
        let now = platform.sample_input();
        if changed(&last, &now) {
            println!("{now:?}");
            if now.left_down && now.drag_change_count != last.drag_change_count {
                println!("drag_paths: {:?}", platform.drag_paths());
            }
        }
        last = now;
    }
}
