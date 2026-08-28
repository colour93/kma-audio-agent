# KMA Audio Agent

`kma-audio-agent` connects one FLOW 8 to KMA Server and owns USB playback, dry Mic1/Mic2 recording, and the supported mixer controls.

## Debian setup

The FLOW 8 must use Recording USB mode at 48 kHz. Install the package, edit `/etc/kma-audio-agent/config.toml`, then run:

`alsa_playback_device` must name a shared ALSA PCM because seamless original/instrumental switching briefly runs two playback pipelines. Use `default` or a card-specific alias such as `default:CARD=F8`; direct `hw:` and `plughw:` playback devices are exclusive and are rejected. The capture device may still use `hw:`.

```bash
sudo -u kma-audio-agent kma-audio-agent probe
sudo -u kma-audio-agent kma-audio-agent calibrate
sudo systemctl enable --now kma-audio-agent
journalctl -u kma-audio-agent -f
```

On first run, journald prints the pairing ID and one-time code. Approve that request from KMA Control. Credentials are stored with mode `0600` under `/var/lib/kma-audio-agent`.

For separate development and production Servers, the existing `--config` option can select a complete profile:

```bash
kma-audio-agent --config /etc/kma-audio-agent/config.dev.toml run
kma-audio-agent --config /etc/kma-audio-agent/config.prod.toml run
```

Each profile must use its own `data_dir`, because the stored Audio Agent credential belongs to one Server. Set `server_url` explicitly in both profiles rather than relying on mDNS when more than one Server is present.

The release build must enable `linux-audio` and use the repository's generic `x86-64` rustflags:

```bash
sudo apt-get update
sudo apt-get install --no-install-recommends \
  build-essential pkg-config libasound2-dev \
  libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev
cargo install cargo-deb --locked
./packaging/build-deb.sh
```

The package and a `SHA256SUMS` file are written to `target/debian/`. Install it with:

```bash
sudo apt install ./target/debian/kma-audio-agent_0.0.0-1_amd64.deb
sudo systemctl enable --now kma-audio-agent
```

GitHub Actions runs the same checks and produces a `kma-audio-agent-debian-amd64` artifact for `v*` tag pushes or a manual `workflow_dispatch` run. The workflow builds on Ubuntu amd64 with `target-cpu=x86-64`; it does not replace FLOW 8/J1900 hardware acceptance.
