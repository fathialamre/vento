//! Shows the native drop catcher over a 300×120 rect at the top centre of
//! the primary screen for 30 s and prints every event. Drop a Finder file,
//! selected text, a link and a web image on it to check each kind by hand.

#[cfg(target_os = "macos")]
fn main() {
    use objc2::{MainThreadMarker, sel};
    use objc2_app_kit::NSApplication;
    use objc2_foundation::NSObjectNSDelayedPerforming;
    use vento_core::geometry::Rect;
    use vento_platform::{CatcherEvent, Dropped};

    const W: f64 = 300.0;
    const H: f64 = 120.0;

    let mtm = MainThreadMarker::new().expect("examples run on the main thread");
    let platform = vento_platform::current();
    platform.prepare_app();
    let Some(primary) = platform.screens().into_iter().next() else {
        eprintln!("no screens");
        return;
    };
    let Some(catcher) = platform.drop_catcher() else {
        eprintln!("no drop catcher on this platform");
        return;
    };
    let rect = Rect {
        x: primary.x + (primary.w - W) / 2.0,
        y: primary.y,
        w: W,
        h: H,
    };
    catcher.on_event(Box::new(|event| match event {
        CatcherEvent::Dropped(Dropped::Image { bytes, ext }) => {
            println!("Dropped(Image {{ ext: {ext:?}, {} bytes }})", bytes.len());
        }
        other => println!("{other:?}"),
    }));
    catcher.show(rect);
    println!("catcher shown at {rect:?} for 30 s");

    let app = NSApplication::sharedApplication(mtm);
    // SAFETY: `terminate:` takes one object argument, and nil is allowed.
    unsafe { app.performSelector_withObject_afterDelay(sel!(terminate:), None, 30.0) };
    app.run();
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("the drop catcher is macOS only");
}
