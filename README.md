# Monoid

Monoid is a desktop audio utility for turning stereo source files into mono WAV outputs.
It is built with Tauri, a small vanilla JavaScript frontend, and a Rust audio pipeline based on Symphonia.

## What It Does

Monoid is aimed at the common "make this usable mono audio" workflow:

- add individual audio files or scan a folder recursively
- queue multiple files in one run
- convert each source into a mono `.wav`
- choose where outputs are written
- control output naming with a filename template
- decide whether existing outputs should be skipped or overwritten
- keep running through bad files or stop on the first failure

The app shows per-file status, overall progress, and batch completion results while the conversion is running.

## Supported Inputs

Monoid accepts a range of common audio container and codec combinations that Symphonia can decode in this project, including:

- `wav`
- `mp3`
- `flac`
- `aac`
- `ogg`
- `m4a`
- `mp4`
- `aiff`
- `caf`
- `mkv`

Outputs are currently written as mono 16-bit WAV files.

## How Conversion Works

For each decoded frame, Monoid averages the available channels into a mono sample, normalizes the result safely, and writes a WAV output. The current implementation is designed for straightforward mono conversion rather than mastering-grade mix decisions.

By default, outputs use the source filename stem with a `_mono.wav` suffix, but this can be changed with the filename template field in the app.

## Development

Install dependencies:

```bash
npm ci
```

Run the frontend tests:

```bash
npm test
```

Run the Rust tests:

```bash
cargo test --manifest-path src-tauri/Cargo.toml
```

Run the desktop app in development:

```bash
npm exec tauri dev
```

Build production bundles for the current platform:

```bash
npm exec tauri build
```

Cross-build the Windows installer from Linux:

```bash
npm exec tauri build -- --target x86_64-pc-windows-gnu
```

## Release Automation

GitHub Actions is configured to:

- run JavaScript and Rust tests on every pull request
- create a tagged release from the current version when the release workflow is triggered manually
- build release bundles for Linux, Windows, and macOS
- publish a GitHub release and attach the generated artifacts
