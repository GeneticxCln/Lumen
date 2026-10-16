/**
 * The English catalog, and the list of keys every other language must supply.
 *
 * Keys are dotted by screen. `{name}` is replaced by a value the caller passes. A message that
 * changes with a count has two entries, `<key>.one` and `<key>.other`, read with `tn`. A message
 * with markup writes it as `<tag>text</tag>`; the component supplies what each tag renders as.
 */
export const en = {
  "common.cancel": "Cancel",
  "common.loading": "Loading…",
  "common.off": "Off",
  "common.playerDefault": "Player default",

  "modal.eyebrow": "LUMEN / YOUR SPACE",
  "modal.close": "Close {title}",

  "lan.label": "Allow sources on my local network",
  "lan.description":
    "Lets Lumen import playlists from this computer or your home network (for example a NAS or a media server). Leave this off unless you host a playlist yourself.",

  "embed.label": "Play video inside the Lumen window (experimental)",
  "embed.description":
    "Shows the video above the controls in this window instead of opening a separate mpv window. Takes effect the next time playback starts.",

  "episodes.title": "Episodes",
  "episodes.season": "Season {season}",
  "episodes.watchedCount": "{watched} of {total} watched",
  "episodes.markSeasonWatched": "Mark season watched",
  "episodes.markSeasonUnwatched": "Mark season unwatched",
  "episodes.markWatched": "Mark watched: {name}",
  "episodes.markUnwatched": "Mark unwatched: {name}",
  "episodes.play": "Play {name}",
  "episodes.row": "E{episode} · {name}",

  "profileGate.eyebrow": "WHO’S WATCHING",
  "profileGate.unlock": "Unlock {name}",
  "profileGate.choose": "Choose a profile",
  "profileGate.subtitle": "Watch history and My List are kept separately for each profile on this device.",
  "profileGate.none": "No profiles are available on this device.",
  "profileGate.tilePin": "{name} · PIN",
  "profileGate.enterPin": "Enter {name}’s PIN",
  "profileGate.unlocking": "Unlocking…",
  "profileGate.unlockButton": "Unlock",
  "profileGate.pinHint": "At least 4 characters.",
  "profileGate.failed": "Could not unlock {name}.",
  "profileGate.note":
    "The PIN locks this profile’s screen on this device. The media library, the database and your files are not encrypted by it.",

  "prefs.title": "Audio and subtitles",
  "prefs.intro": "For the profile you are using. Language choices apply to the next thing you play.",
  "prefs.audioLanguage": "Preferred audio language",
  "prefs.subtitles": "Subtitles",
  "prefs.subtitleSize": "Subtitle size",
  "prefs.subtitlePosition": "Subtitle position",
  "prefs.positionBottom": "bottom",
  "prefs.positionTop": "top",
  "prefs.positionPercent": "{percent}%",
  "prefs.sideloadNote":
    "A subtitle file saved next to a local video (for example <code>Movie.srt</code> beside <code>Movie.mkv</code>) is loaded and shown automatically, and takes priority over subtitles inside the video.",

  "sync.title": "Sync across devices",
  "sync.checking": "Checking sync…",
  "sync.intro":
    "Keep your profiles, My List and watch progress in step on your other computers. There is no account: Lumen makes a secret key on this device, and you type that key into your other devices.",
  "sync.privacy":
    "Everything is encrypted on this device before it is uploaded, so the sync server only holds data it cannot read. Because there is no account, nobody can recover a lost key. If you lose the key and every device that has it, the synced copy is gone. Your library on this computer is never affected.",
  "sync.server.summary": "Sync server: {url}",
  "sync.server.summaryUnset": "Sync server (not set)",
  "sync.server.builtIn":
    "This is the server built into this version of Lumen. You can run your own and enter its address here.",
  "sync.server.enter": "Enter the address of a Lumen sync server. It must start with https://.",
  "sync.server.label": "Server address",
  "sync.server.placeholder": "https://sync.example.com",
  "sync.server.save": "Save server address",
  "sync.on":
    "Sync is on. Your profiles, My List and watch progress follow you across the devices that have your key.",
  "sync.serverLine": "Server: {url}",
  "sync.key.region": "Your sync key",
  "sync.key.new":
    "Write this key down or save it in a password manager now. You need it to add another device, and it cannot be recovered if you lose it.",
  "sync.key.existing":
    "Anyone with this key can read and change your synced data. Only enter it on your own devices.",
  "sync.key.copy": "Copy key",
  "sync.key.copied": "Copied",
  "sync.key.saved": "I have saved it",
  "sync.key.hide": "Hide key",
  "sync.key.copyFailed": "Could not copy to the clipboard. Select the key and copy it by hand.",
  "sync.join.label": "Sync key from your other device",
  "sync.join.placeholder": "XXXX-XXXX-XXXX-…",
  "sync.join.joining": "Joining…",
  "sync.addDevice": "Add this device",
  "sync.turnOffSync": "Turn off sync",
  "sync.turnOffConfirm":
    "Turn off sync on this device? Your library, My List and history stay here. The synced copy stays on the server for your other devices, and you can join again with the key.",
  "sync.deleteData": "Delete synced data",
  "sync.deleteConfirm":
    "Delete the synced copy from the server for every device, and turn sync off here? Your other devices keep their own local data but stop syncing. This cannot be undone.",
  "sync.working": "Working…",
  "sync.syncNow": "Sync now",
  "sync.showKey": "Show key",
  "sync.turnOff": "Turn off",
  "sync.turningOn": "Turning on…",
  "sync.turnOn": "Turn on sync",
  "sync.firstSyncFailed": "Sync is on, but the first sync failed: {reason}",
  "sync.never": "Not synced yet.",
  "sync.last": "Last synced {when}.",
  "sync.pending.one": "{count} change waiting to upload.",
  "sync.pending.other": "{count} changes waiting to upload.",
  "sync.skipped.one":
    "{count} synced record could not be verified and was ignored. The sync server may have altered or damaged it.",
  "sync.skipped.other":
    "{count} synced records could not be verified and were ignored. The sync server may have altered or damaged them.",

  "player.region": "Video player",
  "player.buffering": "Buffering…",
  "player.bufferingLabel": "Buffering",
  "player.backToBrowsing": "Back to browsing",
  "player.back": "Back",
  "player.nextEpisode": "Next episode",
  "player.nextEpisodeTitle": "Next: {title}",

  "transport.position": "Playback position",
  "transport.live": "LIVE",
  "transport.play": "Play",
  "transport.pause": "Pause",
  "transport.playHint": "Play (Space)",
  "transport.pauseHint": "Pause (Space)",
  "transport.back": "Back {seconds} seconds",
  "transport.backHint": "Back {seconds} seconds (Left Arrow)",
  "transport.forward": "Forward {seconds} seconds",
  "transport.forwardHint": "Forward {seconds} seconds (Right Arrow)",
  "transport.mute": "Mute",
  "transport.unmute": "Unmute",
  "transport.muteHint": "Mute / Unmute (M)",
  "transport.volume": "Volume",
  "transport.tracks": "Stream tracks and quality",
  "transport.audio": "Audio",
  "transport.audioTrack": "Audio track",
  "transport.auto": "Auto",
  "transport.subtitles": "Subtitles",
  "transport.subtitleTrack": "Subtitle track",
  "transport.subtitleTools": "Subtitle tools",
  "transport.subtitleAdjustments": "Subtitle adjustments",
  "transport.subtitleDelay": "Subtitle delay",
  "transport.subtitlesEarlier": "Show subtitles earlier",
  "transport.subtitlesEarlierHint": "Subtitles earlier",
  "transport.subtitlesLater": "Show subtitles later",
  "transport.subtitlesLaterHint": "Subtitles later",
  "transport.subtitlesSmaller": "Smaller subtitles",
  "transport.subtitlesSmallerGlyph": "A−",
  "transport.subtitlesLarger": "Larger subtitles",
  "transport.subtitlesLargerGlyph": "A+",
  "transport.addSubtitle": "Add a subtitle file",
  "transport.addSubtitleGlyph": "＋ File",
  "transport.quality": "Quality",
  "transport.qualityLabel": "Playback quality",
  "transport.qualityUnavailable": "Unavailable",
  "transport.stop": "Stop playback",
  "transport.fullscreen": "Fullscreen",
  "transport.fullscreenHint": "Fullscreen (F)",
  "transport.exitFullscreen": "Exit fullscreen",
  "transport.exitFullscreenHint": "Exit fullscreen (F)",
} as const;
