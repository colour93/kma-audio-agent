# KMA Audio Agent

`kma-audio-agent` connects one FLOW 8 to KMA Server and owns USB playback, dry Mic1/Mic2 recording, and the supported mixer controls.

## Debian setup

The FLOW 8 must use Recording USB mode at 48 kHz. Install the package, edit `/etc/kma-audio-agent/config.toml`, then run:

```bash
sudo -u kma-audio-agent kma-audio-agent probe
sudo -u kma-audio-agent kma-audio-agent calibrate
sudo systemctl enable --now kma-audio-agent
journalctl -u kma-audio-agent -f
```

On first run, journald prints the pairing ID and one-time code. Approve that request from KMA Control. Credentials are stored with mode `0600` under `/var/lib/kma-audio-agent`.

The release build must enable `linux-audio` and use the repository's generic `x86-64` rustflags:

```bash
cargo build --release --features linux-audio
cargo deb --no-build
```
