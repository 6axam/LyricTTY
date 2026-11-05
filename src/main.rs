mod app;
mod frame_pacing;

use anyhow::Result;
use app::App;
use crossterm::{
    event::{self, Event},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use frame_pacing::FramePacer;
use lyrictty::{
    assets::AssetService,
    config::{self, Config},
    playback::{self, service::PlaybackService, PlaybackBackend},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::{
    env, io,
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Default)]
struct Options {
    debug: bool,
    demo: bool,
    url: Option<String>,
    lyrics: Option<PathBuf>,
}
fn options(args: Vec<String>) -> Result<Options> {
    let mut options = Options::default();
    let mut args = args.into_iter();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--debug" => options.debug = true,
            "--demo" => options.demo = true,
            "--lyrics" => {
                options.lyrics = Some(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("--lyrics requires a file"))?
                        .into(),
                )
            }
            "--native" => {
                options.url = Some(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("--native requires a URL"))?,
                )
            }
            _ if a.starts_with('-') => anyhow::bail!("Unknown option: {a}"),
            _ => {
                if options.url.is_some() {
                    anyhow::bail!("Only one startup URL is accepted");
                }
                options.url = Some(a);
            }
        }
    }
    if options.demo && options.url.is_some() {
        anyhow::bail!("--demo cannot be combined with a playback URL");
    }
    Ok(options)
}
struct ScreenGuard;
impl Drop for ScreenGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            crossterm::terminal::EndSynchronizedUpdate,
            event::DisableMouseCapture,
            event::DisableBracketedPaste,
            LeaveAlternateScreen,
            crossterm::cursor::Show
        );
    }
}

fn main() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.first().is_some_and(|a| a == "doctor") {
        return app::doctor();
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("LyricTTY — development / pre-release\n\nlyrictty [--debug] [--lyrics FILE] [URL or LOCAL_FILE]\nlyrictty --native URL\nlyrictty --demo\nlyrictty doctor\n\nDefault: auto-discover MPRIS players. Press o to open native media.\nLocal lyrics support enhanced LRC, Lyricsfile JSON and YAML.\n--demo is an authored, silent UI fixture.");
        return Ok(());
    }
    let options = options(args)?;
    let config = Config::load(&config::config_path())?;
    let mut app = App::new(config.clone(), options.debug);
    app.startup(options.url, options.lyrics, options.demo);
    app.asset_request = Some(AssetService::start(config, config::cache_path()));
    app.service = Some(PlaybackService::start(move || {
        if options.demo {
            return (
                vec![Box::new(playback::demo::DemoBackend::default()) as Box<dyn PlaybackBackend>],
                Vec::new(),
            );
        }
        let mut backends: Vec<Box<dyn PlaybackBackend>> =
            vec![Box::new(playback::native::NativeBackend::new())];
        let mut errors = Vec::new();
        match playback::mpris::MprisBackend::new() {
            Ok(b) => backends.push(Box::new(b)),
            Err(_) => errors.push("D-Bus unavailable · native playback is available".into()),
        };
        (backends, errors)
    }));
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            crossterm::terminal::EndSynchronizedUpdate,
            event::DisableMouseCapture,
            LeaveAlternateScreen,
            crossterm::cursor::Show
        );
        previous(info);
    }));
    enable_raw_mode()?;
    let _guard = ScreenGuard;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        event::EnableMouseCapture,
        event::EnableBracketedPaste
    )?;
    app.picker = ratatui_image::picker::Picker::from_query_stdio().ok();
    app.setup_pixel_lyrics();
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    let result = run(&mut terminal, &mut app);
    app.cleanup_graphics(&mut io::stdout())?;
    result
}

fn run<B: ratatui::backend::Backend>(terminal: &mut Terminal<B>, app: &mut App) -> Result<()> {
    let mut pacer = FramePacer::new(Instant::now(), u32::from(app.config.fps));
    let mut idle = true;
    let synchronized = app
        .picker
        .as_ref()
        .is_some_and(|p| p.protocol_type() == ratatui_image::picker::ProtocolType::Kitty);
    loop {
        app.tick();
        let now = Instant::now();
        let active = app.dirty || app.animating();
        if active && idle {
            pacer.reset(now);
        }
        idle = !active;
        if app.dirty && pacer.wait(now).is_zero() {
            if synchronized {
                execute!(io::stdout(), crossterm::terminal::BeginSynchronizedUpdate)?;
            }
            let draw = (|| -> Result<()> {
                terminal.draw(|f| app.draw(f))?;
                app.flush_graphics(&mut io::stdout())?;
                Ok(())
            })();
            if synchronized {
                execute!(io::stdout(), crossterm::terminal::EndSynchronizedUpdate)?;
            }
            draw?;
            pacer.presented(Instant::now(), u32::from(app.config.fps));
        }
        let wait = if app.animating() || app.dirty {
            pacer.wait(Instant::now())
        } else {
            Duration::from_millis(200)
        };
        if event::poll(wait)? {
            match event::read()? {
                Event::Key(key) if key.kind != event::KeyEventKind::Release => app.handle_key(key),
                Event::Mouse(mouse) => app.handle_mouse(mouse),
                Event::Resize(_, _) => app.resize_graphics(),
                Event::Paste(text) => app.handle_paste(&text),
                _ => {}
            }
        }
        if app.should_quit {
            break;
        }
    }
    Ok(())
}
