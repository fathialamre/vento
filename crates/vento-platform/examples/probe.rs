//! Prints the screens, the Pocket island home and the probe binary's Finder
//! icon size, then platform input changes for 15 s so the sampler can be
//! checked by hand: drag a Finder file, press Ctrl, press Esc, release.

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
    let end = Instant::now() + Duration::from_secs(15);
    while Instant::now() < end {
        std::thread::sleep(Duration::from_millis(8));
        let now = platform.sample_input();
        if changed(&last, &now) {
            println!("{now:?}");
        }
        last = now;
    }
}
