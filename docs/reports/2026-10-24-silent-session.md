# Silent session before the 2026-10-24 reboot

Read-only investigation, 2026-10-24. Sources: `journalctl -b -1` (previous boot, 2026-10-19 21:46 to 2026-10-24 01:08:39 MSK) as the owner's user, plus read-only checks of the current boot's audio configuration. Nothing was killed, restarted, built or written to the live session.

## Limits of the evidence (read first)

- The investigating account is not in `adm`/`systemd-journal`. `journalctl` printed "You are currently not seeing messages from other users and the system." Kernel messages (`-k`: OOM killer, SOF DSP errors, ALSA, suspend/resume), system units (systemd-oomd, logind, upower) and the system journal were NOT visible. I did not use sudo because the project rules reserve it for package installs and host tuning.
- Zero journal entries exist for `pipewire`, `wireplumber`, `pipewire-pulse` or `speech-dispatcher` in boot -1 (`journalctl -b -1 --user -u <unit>` is empty for all four). In the current boot the same units do log their start-up lines. So either they logged nothing for the whole boot or the start-up lines were rotated out. Either way, no audio-stack error was recorded in what I can read.
- The conclusion below is therefore a ranked inference, not a proven cause.

## Timeline (local time, MSK = UTC+3)

| Time | Event |
|---|---|
| 10-23 11:59 | `vte-spawn-...scope: A process of this unit has been killed by the OOM killer.` Only OOM line in the whole boot, 13 h before the incident, in a terminal scope. |
| 10-23 22:40 - 23:10 | Many gorca restarts (dev iteration). Every start logs `WARN gorca_daemon: speech backend changed notice=Fallback { from: "voicego", to: "speech-dispatcher" }`. |
| 10-23 23:02 | Owner clears `/tmp` (`du -sxh /tmp`, `lsof +D /tmp/gmo*`, removal of `/tmp/org.chromium.*`). So the 85-88% tmpfs state was being handled at this point. |
| 10-23 23:10:38 | gorca.service (pid 1109396) starts; "keyboard monitor active", "atspi connector running". |
| 10-23 23:12, 23:31, 23:55 | Private test dbus-brokers (`dbus-broker[1127945]`, `[1167604]`, `[1220207]`): `UID 1000 exceeded its 'bytes' quota`, peers `:1.1`/`:1.2` disconnected. These are throw-away test rigs, not the live bus (different broker pids, each followed by a fresh `dbus-broker-launch ... Ready`). Evidence of event floods in tests only. |
| 10-24 00:39:33 | `gorca-toggle.sh` (via gsd-media-keys) stops gorca cleanly: `SIGTERM received`, `gorca stopped`; `speech lane stats=SpeechStats { spoken: 264, replaced: 17, stale_dropped: 7, ... inbox_dropped: 0 ...}`. Memory peak 89.6M, swap peak 74.2M. |
| 10-24 00:39:34 | gorca.service restarts (pid 1245531), same voicego->speech-dispatcher fallback warning. |
| 10-24 00:48-00:49 | Two Chromium scopes started (e2e tests). |
| 10-24 01:04:03, 01:04:15 | `sudo tee /sys/class/backlight/intel_backlight/brightness` twice from pts/4 (owner or a script dimming the screen). |
| 10-24 01:04:36 | Last `gorca_phase` line (`phase="web_focus"`). |
| 10-24 01:05:33 | Last gorca log line: `input lane statistics keys=5266` (was 5254 a minute earlier, so about 12 keystrokes). |
| 10-24 01:05:33 - 01:08:39 | No gorca output at all. Only Firefox `impl_GetText: assertion 'ATK_IS_TEXT (user_data)' failed` warnings (noise present all night). Journal ends 01:08:39 (forced reboot). |
| 10-24 01:23:16 | New boot. pipewire, wireplumber, pipewire-pulse start; speech-dispatcher starts 01:23:26. |

## What was checked and found clean

- **OOM / memory pressure:** the user manager logged no OOM kill after 11:59, no systemd-oomd line, no gnome-shell or gorca kill. Kernel OOM messages are not readable (see limits). Earlier in the boot Firefox peaked at 7.7 GB with a 9.4 GB swap peak, so swap is used heavily on this machine; gorca itself shows 70-110 MB swap peaks per run (its pages get swapped out, a separate latency risk).
- **/tmp full:** no `ENOSPC`/`No space left` line in the user journal. `/tmp` was cleaned at 23:02. Unverified whether it refilled by 01:08.
- **gnome-shell:** no crash or freeze message; gorca-helper lines from the same shell pid appear until 00:48.
- **gorca:** no panic, no ERROR line, no stall warning. The last events are normal.
- **speech-dispatcher / voicego / espeak-ng:** no log lines in boot -1 from any of them. voicego was never in use: every gorca start in this boot fell back to speech-dispatcher.

## Was gorca alive and trying to speak at the end?

Unproven either way. gorca only logs key statistics when the key count changes, and logs `gorca_phase` on web focus events, so an idle gorca produces no lines. The absence of lines after 01:05:33 is consistent with the owner being away. Equally, if gorca had tried to speak and the backend had failed, it would likely not have logged it at INFO (there is no per-utterance or backend-health logging). That is itself a finding: gorca cannot tell the owner or the journal that its speech output has gone dead.

## Most likely root cause (confidence: low-medium)

**The audio path, not gorca, failed, and it is a single point of failure that nothing recovers.**

Evidence from the configuration (current boot, files dated 2026-10-02):

- `/etc/pipewire/pipewire.conf.d/51-espeakup-dmix.conf` defines the only usable sink as an ALSA `dmix` sink on the Intel SOF DSP card: `api.alsa.path = "dmix:CARD=sofhdadsp,DEV=0"`, "Internal Speakers (shared dmix)".
- `/etc/wireplumber/wireplumber.conf.d/51-espeakup-dmix.conf` sets `device.disabled = true` for the same card (`alsa_card.pci-0000_00_1f.3-platform-skl_hda_dsp_generic`). wireplumber logged `ALSA card/device ... disabled` at 01:23:24. `wpctl status` shows one sink only (the dmix one) plus the NVIDIA HDMI controller with no sink.
- `/etc/asound.conf` routes the ALSA default through the same dmix/dsnoop on `sofhdadsp`.

Consequence: every sound source (the `speech-dispatcher-espeak-ng` PipeWire client, browsers, gnome-shell) shares one hand-made PCM on a SOF DSP. The card is disabled in wireplumber, so wireplumber's device and profile management, hotplug and re-enumeration do not apply to it. If the SOF DSP hangs, the PCM goes SUSPENDED/-EIO/-ENODEV, or a client wedges the dmix buffer, the pw ALSA PCM node errors and nothing re-creates it. Result: total silence for all applications while gorca stays healthy and logs nothing, recoverable only by restarting pipewire (which a blind user cannot reasonably do) or rebooting. This matches "no sound at all, rebooted".

What would confirm it (needs `adm` or sudo read access, which I did not use): `journalctl -b -1 -k | grep -iE 'sof|snd_|hda|dsp|xrun|suspend'` and `journalctl -b -1 -u systemd-logind -u systemd-oomd`. Look for `sof-audio-pci` firmware errors, `DSP panic`, `IPC timeout`, or an idle suspend around 01:05-01:08 after the 01:04 backlight dimming.

Alternatives, less likely: (a) runtime power management of the SOF/HDA codec leaving the PCM dead after idle (consistent with the 01:04 dimming; the kernel log would show it); (b) a speech-dispatcher or espeak-ng module hang (silences only speech; the owner reported all sound, though that may have meant speech only); (c) kernel OOM or tmpfs-full (no user-visible trace and no support in the readable journal).

## Preventions

1. **gorca speech-health watchdog (highest value).** Detect a dead output path: an end-to-end probe (short utterance, require the backend's end-of-speech event within a deadline), a periodic INFO heartbeat with `spoken/failed/last_ok_age`, and on failure reconnect the speech client, retry through a second backend, and escalate. Also use a non-audio channel (braille if present).
2. **One-key audio recovery.** A documented, explicit owner-triggered helper that restarts only the user units `pipewire`, `wireplumber`, `pipewire-pulse` and `speech-dispatcher` (not the session). It turns a forced reboot into a one-key recovery. It must never run from tests.
3. **Remove the single point of failure in the audio config.** Re-evaluate `51-espeakup-dmix.conf` on both sides: let wireplumber manage the SOF card or add a second sink and a default-sink fallback. If dmix is required for espeakup, review pw ALSA node error recovery and add a watchdog for the sink.
4. **Persistent diagnostics.** Add the owner to `systemd-journal` (and `adm`) so agents can read kernel and system logs; make sure pipewire/wireplumber/speech-dispatcher output survives in the user journal; log speech-backend errors in gorca at WARN (rate-limited); log why voicego failed to start (it fell back on every start in boot -1).
5. **Build and test load limits.** Run test sessions in a transient cgroup below the live session (`systemd-run --user --scope -p MemoryMax=8G -p CPUWeight=20 -p IOWeight=20 ...`); keep cargo jobs capped (already 6) and cap concurrent Firefox/Chromium e2e. The private dbus-brokers hit the `bytes` quota three times (23:12, 23:31, 23:55); keep flood tests in the private world.
6. **/tmp cap.** Put test workdirs on a dedicated size-limited tmpfs or a quota'd directory instead of the shared `/tmp`; clean `/tmp/org.chromium.*`, `/tmp/gmo*` and `*.zip` leftovers at the end of each run; refuse to start e2e when `/tmp` is above 70%.
7. **Swap hygiene for the reader.** gorca shows 70-110 MB swap peaks per run. Set `MemorySwapMax=0` or `MemoryLow=` on `gorca.service` (or `mlockall` the hot set) so host memory pressure cannot page the reader out.
