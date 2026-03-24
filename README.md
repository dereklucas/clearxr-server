# Clear XR Server

Clear XR Server lets you stream interactive virtual worlds and applications from high-powered gaming PCs, workstations, or servers, to spatial computing, aka. extended reality (XR) devices, starting with Apple Vision Pro.  

Clear XR utilizes dynamic foveated streaming to render the sharpest content in the user’s direct line of sight. 

Currently only OpenXR compliant applications are supported. OpenVR (and SteamVR) applications are not yet supported.

# For End Users 

Download the [latest release](https://github.com/clear-xr/clearxr-server/releases)

Download the client from Test Flight

[![Clear XR](https://i.imgur.com/DHhfmmK.png)](https://testflight.apple.com/join/ed6778fF)

# For Developers & Hackers

Clear XR currently is built around a visionOS app, Windows desktop app, an OpenXR test application, and an OpenXR API layer, and Apple's FoveatedStreaming framework for visionOS.   

It may expand to other mobile devicdes (e.g. iOS and iPad), headsets (e.g. Quest and Steam Frame), streaming frameworks (e.g. Cloud XR native, or ALVR), and operating systems (e.g. Linux, and perhaps some day macOS).  Discussions and contributions are welcome.

# Prerequisites
## Clear XR Server
- Windows 10/11 PC with NVIDIA 40xx (Ada), 50xx (Blackwell), or other Ada/Blackwell GPUs (L40, L40S, RTX 5000/6000 series and 5000/6000 pro).  
- Note that NVIDIA only tests Cloud XR on 4090, 5080, and 5090 consumer GPUs.  The authors have also tested successfully on a 5070 Ti (RIP).  **Clear XR cannot run on NVIDIA 30xx (Ampere) cards.**  
- Rust and its prerequisites, which include the Microsoft Visual Studio 2022 build tools. 
- NVIDIA Cloud XR and Stream Manager SDKs, described below.


## Clear XR Supported Client Devices
- Apple Vision Pro M2 or M5, running visionOS 26.4 or higher

# Organization

The repository is organized into three main Rust subcomponents:

- `clearxr-streamer`
  The desktop server application. This is the main control surface for Clear XR
  Server and owns the Tauri shell, session-management service, Bonjour
  advertising, CloudXR process control, and pairing flow.
- `clearxr-space`
  A native OpenXR application used as the default Clear XR landing space to test out controllers.  It may eventually evolve into an App Launcher.  It can  run against the CloudXR runtime and acts as the streamed OpenXR content.
- `clearxr-layer`
  An OpenXR API layer that injects controller, haptics, configuration, and related runtime data into the
  Clear XR stack.

Supporting files live alongside those components:

- `ui/`
  Frontend assets used by `clearxr-streamer`.
- `scripts/`
  Local helper scripts, including vendor reconstruction.
- `vendor/`
  Locally rebuilt NVIDIA CloudXR runtime and Stream Manager files. This is not
  intended to be redistributed through the source repository.
- `xtask/`
  Rust Cargo tasks for building the full repository.


## How do I build this?
1. Install Rust via the [Rustup install documentation](https://rust-lang.org/tools/install/)
2. Download the NVIDIA Cloud XR dependencies, described below.
3. Run the `scripts\build-vendor.ps1` vendor script.
4. Build with `cargo`

## Downloading NVIDIA Cloud XR and assembling the vendor directory

We redistribute the Clear XR binaries with the NVIDIA Cloud XR binaries for only end-user purposes.  The NVIDIA CloudXR binaries cannot be redistributed for development purposes.  If you want to build Clear XR yourself, or hack on it, you must agree to the NVIDIA license terms on the NGC and download yourself.  We've provided a handly script to build the `vendor/` locally from the official NVIDIA downloads.

Download these two archives from NGC:

- [CloudXR Runtime 6.0.4](https://catalog.ngc.nvidia.com/orgs/nvidia/resources/cloudxr-runtime?version=6.0.4)
- [Stream Manager 6.0.3](https://catalog.ngc.nvidia.com/orgs/nvidia/resources/cloudxr-stream-manager?version=6.0.3)

Place the downloaded zip files in the repository root, then run:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\build-vendor.ps1
```

The script will:

- extract the Stream Manager package into the top-level `vendor/` layout used by
  the app,
- extract the nested CloudXR Win64 SDK zip into
  `vendor/Server/releases/<version>/`,
- preserve repo-owned overlay files that already exist only in `vendor/`, such
  as `vendor/Server/cloudxr-runtime.yaml`.

## Main App

`clearxr-streamer` is the main application entry point for the project. It uses
the frontend in `ui/` and stages the expected CloudXR files from `vendor/`
during build and runtime startup.

## Build

Build the full project from the repository root with:

```powershell
cargo xtask build
```

For a release build:

```powershell
cargo xtask build --release
```

The xtask builds the components in this order:

1. `clear-xr`
2. `clear-xr-layer`
3. `clearxr-streamer`

That order ensures `clearxr-streamer` can stage related
runtime files from the expected local build outputs.

## Licensing

The original source code in this repository is licensed under the MIT License.
See [LICENSE](LICENSE).

Third-party software is not covered by the MIT License. In particular, NVIDIA
CloudXR components and files are subject to NVIDIA's separate
license terms. See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
