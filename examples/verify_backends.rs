//! Opt-in live checks. CI does not execute examples or use public services.
use anyhow::{ensure, Result};
use lyrictty::{
    artwork::ArtworkProvider,
    playback::{
        mpris::MprisBackend,
        native::{NativeBackend, NativeOptions},
        PlaybackBackend, PlaybackCommand, PlaybackSnapshot, PlaybackStatus, Track,
    },
    providers::{LyricsCache, LyricsService},
};
use std::{
    thread,
    time::{Duration, Instant},
};
fn until(
    backend: &mut dyn PlaybackBackend,
    predicate: impl Fn(&PlaybackSnapshot) -> bool,
    seconds: u64,
) -> Result<PlaybackSnapshot> {
    let end = Instant::now() + Duration::from_secs(seconds);
    loop {
        let snapshots = backend.refresh()?;
        if let Some(s) = snapshots.into_iter().find(|s| predicate(s)) {
            return Ok(s);
        }
        ensure!(
            Instant::now() < end,
            "Timed out waiting for confirmed player state"
        );
        thread::sleep(Duration::from_millis(100));
    }
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str){
        Some("lyrics")=>{
            ensure!(args.len()>=3,"lyrics TITLE ARTIST [DURATION]");let cache=tempfile::tempdir()?;let service=LyricsService::new(Some(LyricsCache::new(cache.path())))?;
            let track=Track{title:args[1].clone(),artist:args[2].clone(),duration:args.get(3).and_then(|v|v.parse().ok()).unwrap_or(0.0),..Default::default()};
            let d=service.lookup(&track)?;println!("{} — {}: {} · {} lines · {} words",track.artist,track.title,d.quality_label(),d.lines.len(),d.lines.iter().map(|l|l.words.len()).sum::<usize>());
            let cached=service.lookup(&track)?;ensure!(cached.cached,"Expected cached lookup");println!("Second lookup: cache confirmed");
        },
        Some("native")=>{
            ensure!(args.len()==2,"native URL_OR_FILE");let mut backend=NativeBackend::with_options(NativeOptions{extra_args:vec!["--ao=null".into()],..Default::default()});
            backend.command("native",PlaybackCommand::Open(args[1].clone()))?;
            let s=until(&mut backend,|s|s.status==PlaybackStatus::Playing&&s.track.duration>0.0,90)?;
            println!("Loaded: {} — {} · {:.1}s · artwork {}",s.track.artist,s.track.title,s.track.duration,s.track.artwork_url.is_some());
            backend.command("native",PlaybackCommand::Pause)?;until(&mut backend,|s|s.status==PlaybackStatus::Paused,3)?;println!("Pause confirmed");
            backend.command("native",PlaybackCommand::Volume(0.25))?;until(&mut backend,|s|(s.volume-0.25).abs()<0.02,3)?;println!("Volume confirmed");
            if s.capabilities.seek {let target=30.0f64.min(s.track.duration/2.0);backend.command("native",PlaybackCommand::SetPosition(target))?;until(&mut backend,|s|(s.position_now()-target).abs()<2.0,5)?;println!("Absolute seek confirmed");}
            backend.command("native",PlaybackCommand::Play)?;until(&mut backend,|s|s.status==PlaybackStatus::Playing,3)?;println!("Play confirmed");
            if let Some(url)=s.track.artwork_url {let dir=tempfile::tempdir()?;let art=ArtworkProvider::new(dir.path().into())?.load(&url)?;println!("Artwork decoded: {}×{} · text contrast {:.1}",art.image.width(),art.image.height(),lyrictty::theme::contrast(art.palette.fg,art.palette.bg));}
        },
        Some("mpris")=>{
            let mut backend=MprisBackend::new()?;let sources=backend.refresh()?;for source in &sources{println!("{} · {} · {:?} · seek {}",source.source_id,source.source_name,source.status,source.capabilities.seek);}
            if let Some(id)=args.get(1){let initial=sources.iter().find(|s|&s.source_id==id).ok_or_else(||anyhow::anyhow!("Requested player not found"))?.clone();
                backend.command(id,PlaybackCommand::PlayPause)?;let target=if initial.status==PlaybackStatus::Playing{PlaybackStatus::Paused}else{PlaybackStatus::Playing};until(&mut backend,|s|&s.source_id==id&&s.status==target,5)?;println!("MPRIS PlayPause confirmed");
                if initial.capabilities.seek{backend.command(id,PlaybackCommand::SetPosition(8.0))?;until(&mut backend,|s|&s.source_id==id&&(s.position_now()-8.0).abs()<2.0,5)?;println!("MPRIS SetPosition confirmed");}
                backend.command(id,if initial.status==PlaybackStatus::Playing{PlaybackCommand::Play}else{PlaybackCommand::Pause})?;
            }
        },_=>println!("verify_backends lyrics TITLE ARTIST [DURATION]\nverify_backends native URL_OR_FILE\nverify_backends mpris [EXPLICIT_SOURCE_TO_EXERCISE]")
    }
    Ok(())
}
