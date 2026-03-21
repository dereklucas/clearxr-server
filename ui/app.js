const runtimeIds = {
  overall: document.getElementById("overall-status"),
  bonjour: document.getElementById("bonjour-status"),
  session: document.getElementById("session-status"),
  cloudxr: document.getElementById("cloudxr-status"),
  serverId: document.getElementById("server-id"),
  detail: document.getElementById("runtime-detail"),
  bonjourDetail: document.getElementById("bonjour-detail"),
  sessionDetail: document.getElementById("session-detail"),
  cloudxrDetail: document.getElementById("cloudxr-detail"),
  serverToggle: document.getElementById("server-toggle"),
  qrDrawer: document.getElementById("qr-drawer"),
  qrFrame: document.getElementById("qr-frame"),
  qr: document.getElementById("qr-preview"),
  qrStatus: document.getElementById("qr-status"),
  qrDrawerStatus: document.getElementById("qr-drawer-status"),
  localAddress: document.getElementById("local-address"),
  port: document.getElementById("session-port"),
  forceQrCode: document.getElementById("force-qr-code"),
  openxrRuntime: document.getElementById("openxr-runtime-status"),
  openxrRuntimeDetail: document.getElementById("openxr-runtime-detail"),
  openxrLayer: document.getElementById("openxr-layer-status"),
  openxrLayerDetail: document.getElementById("openxr-layer-detail"),
  openxrPanel: document.getElementById("openxr-panel"),
  openxrSummaryStatus: document.getElementById("openxr-summary-status"),
  openxrAction: document.getElementById("openxr-action"),
  openxrDeregisterLayer: document.getElementById("openxr-deregister-layer"),
  openxrActionNote: document.getElementById("openxr-action-note"),
};

const invoke =
  window.__TAURI__ &&
  window.__TAURI__.core &&
  typeof window.__TAURI__.core.invoke === "function"
    ? window.__TAURI__.core.invoke.bind(window.__TAURI__.core)
    : null;

let currentBundleId = "";
let currentSnapshot = null;
let currentOpenXrStatus = null;
let serverActionInFlight = false;
let openXrActionInFlight = false;

function formatHealthLabel(health) {
  switch (health) {
    case "running":
      return "Running";
    case "paused":
      return "Listening";
    case "stopped":
    default:
      return "Stopped";
  }
}

function setRuntimeFallback(message) {
  runtimeIds.overall.textContent = "Unavailable";
  runtimeIds.bonjour.textContent = "Unavailable";
  runtimeIds.session.textContent = "Unavailable";
  runtimeIds.cloudxr.textContent = "Unavailable";
  runtimeIds.serverId.textContent = "Unavailable";
  runtimeIds.detail.textContent = message;
  runtimeIds.bonjourDetail.textContent = message;
  runtimeIds.sessionDetail.textContent = message;
  runtimeIds.cloudxrDetail.textContent = message;
  runtimeIds.qrDrawer.classList.add("is-hidden");
  runtimeIds.qrDrawer.classList.remove("is-visible");
  runtimeIds.qr.removeAttribute("src");
  runtimeIds.qrStatus.textContent =
    "Open this UI from the Tauri shell to show pairing state.";
  runtimeIds.qrDrawerStatus.textContent =
    "Open this UI from the Tauri shell to show pairing state.";
  runtimeIds.openxrRuntime.textContent = "Unavailable";
  runtimeIds.openxrRuntimeDetail.textContent = message;
  runtimeIds.openxrLayer.textContent = "Unavailable";
  runtimeIds.openxrLayerDetail.textContent = message;
  runtimeIds.openxrAction.textContent = "Unavailable";
  runtimeIds.openxrAction.disabled = true;
  runtimeIds.openxrDeregisterLayer.disabled = true;
  runtimeIds.openxrActionNote.textContent = message;
}

function setAddressOptions(addresses, selectedAddress) {
  runtimeIds.localAddress.replaceChildren();

  for (const address of addresses) {
    const option = document.createElement("option");
    option.value = address;
    option.textContent = address;
    runtimeIds.localAddress.appendChild(option);
  }

  if (selectedAddress) {
    runtimeIds.localAddress.value = selectedAddress;
  }

  if (!runtimeIds.localAddress.value && addresses.length > 0) {
    runtimeIds.localAddress.value = addresses[0];
  }
}

function isServerRunning(snapshot) {
  if (!snapshot) {
    return false;
  }

  return (
    snapshot.sessionManagement.health !== "stopped" ||
    snapshot.bonjour.health !== "stopped"
  );
}

function renderServerButton(snapshot) {
  const running = isServerRunning(snapshot);
  runtimeIds.serverToggle.textContent = running ? "Stop Server" : "Start Server";
  runtimeIds.serverToggle.classList.toggle("secondary", running);
  runtimeIds.serverToggle.disabled = serverActionInFlight;
  runtimeIds.serverToggle.classList.toggle("button-busy", serverActionInFlight);

  runtimeIds.localAddress.disabled = running || serverActionInFlight;
  runtimeIds.port.disabled = running || serverActionInFlight;
  runtimeIds.forceQrCode.disabled = running || serverActionInFlight;
}

function renderQrDrawer(snapshot) {
  if (snapshot.qrDataUrl) {
    runtimeIds.qr.src = snapshot.qrDataUrl;
    runtimeIds.qrDrawer.classList.remove("is-hidden");
    runtimeIds.qrDrawer.classList.add("is-visible");
    runtimeIds.qrStatus.textContent =
      "Pairing is active. The QR drawer is open on the right.";
    runtimeIds.qrDrawerStatus.textContent =
      "Scan this QR code from the Vision Pro to continue pairing.";
  } else {
    runtimeIds.qr.removeAttribute("src");
    runtimeIds.qrDrawer.classList.remove("is-visible");
    runtimeIds.qrDrawer.classList.add("is-hidden");
    runtimeIds.qrStatus.textContent =
      "The pairing QR will slide out only when the Vision Pro requests it.";
    runtimeIds.qrDrawerStatus.textContent =
      "The QR code will appear here when pairing is requested.";
  }
}

function isOpenXrReady(status) {
  return Boolean(status?.runtimeIsActive && status?.layerIsRegistered);
}

function renderOpenXrButtons(status) {
  let label = "Register OpenXR Runtime + Layer";
  if (status?.runtimeIsActive && !status?.layerIsRegistered) {
    label = "Register OpenXR Layer";
  } else if (isOpenXrReady(status)) {
    label = "Reapply OpenXR Registration";
  }

  runtimeIds.openxrAction.textContent = label;
  runtimeIds.openxrAction.disabled = openXrActionInFlight;
  runtimeIds.openxrAction.classList.toggle("button-busy", openXrActionInFlight);
  runtimeIds.openxrAction.classList.toggle("secondary", isOpenXrReady(status));

  runtimeIds.openxrDeregisterLayer.disabled =
    openXrActionInFlight || !status?.layerIsRegistered;
  runtimeIds.openxrDeregisterLayer.classList.toggle(
    "button-busy",
    openXrActionInFlight,
  );
}

function renderOpenXrStatus(status) {
  currentOpenXrStatus = status;

  const fullyRegistered = isOpenXrReady(status);
  runtimeIds.openxrPanel.classList.toggle("is-hidden", fullyRegistered);

  if (!fullyRegistered) {
    runtimeIds.openxrSummaryStatus.textContent = status.runtimeIsActive
      ? "Layer Needed"
      : "Needs Registration";
  }

  runtimeIds.openxrRuntime.textContent = status.runtimeIsActive
    ? "Clear XR Active"
    : "Needs Registration";
  runtimeIds.openxrRuntimeDetail.textContent = status.runtimeDetail;
  runtimeIds.openxrLayer.textContent = status.layerIsRegistered
    ? "Registered"
    : "Needs Registration";
  runtimeIds.openxrLayerDetail.textContent = status.layerDetail;
  runtimeIds.openxrActionNote.textContent = status.runtimeIsActive
    ? "Clear XR is already selected as the OpenXR runtime. The button can still reapply the layer/runtime registration if needed."
    : "Runtime registration writes the machine-wide OpenXR ActiveRuntime key, so Windows may require administrator rights.";
  renderOpenXrButtons(status);
}

function renderSnapshot(snapshot, options = {}) {
  const { syncForm = false } = options;

  currentSnapshot = snapshot;
  currentBundleId = snapshot.config.bundleId || currentBundleId;
  runtimeIds.overall.textContent = isServerRunning(snapshot)
    ? "Running"
    : "Stopped";
  runtimeIds.bonjour.textContent = formatHealthLabel(snapshot.bonjour.health);
  runtimeIds.session.textContent = formatHealthLabel(
    snapshot.sessionManagement.health,
  );
  runtimeIds.cloudxr.textContent = formatHealthLabel(snapshot.cloudxr.health);
  runtimeIds.serverId.textContent = snapshot.serverId || "Not generated";
  runtimeIds.detail.textContent = `Bundle: ${snapshot.config.bundleId} | Host: ${snapshot.config.hostAddress}:${snapshot.config.port}`;
  runtimeIds.bonjourDetail.textContent = `Bonjour: ${snapshot.bonjour.detail}`;
  runtimeIds.sessionDetail.textContent = `Session: ${snapshot.sessionManagement.detail}`;
  runtimeIds.cloudxrDetail.textContent = `CloudXR: ${snapshot.cloudxr.detail}`;

  if (syncForm) {
    if (snapshot.config.hostAddress) {
      const existing = Array.from(runtimeIds.localAddress.options).map(
        (option) => option.value,
      );
      if (!existing.includes(snapshot.config.hostAddress)) {
        setAddressOptions(
          [...existing, snapshot.config.hostAddress],
          snapshot.config.hostAddress,
        );
      } else {
        runtimeIds.localAddress.value = snapshot.config.hostAddress;
      }
    }

    runtimeIds.port.value = snapshot.config.port;
    runtimeIds.forceQrCode.checked = snapshot.config.forceQrCode;
  }

  renderQrDrawer(snapshot);
  renderServerButton(snapshot);
}

async function refreshSnapshot() {
  if (!invoke) {
    return;
  }

  const [snapshot, openxrStatus] = await Promise.all([
    invoke("get_runtime_snapshot"),
    invoke("get_openxr_registration_status"),
  ]);
  renderSnapshot(snapshot);
  renderOpenXrStatus(openxrStatus);
}

async function bootstrap() {
  if (!invoke) {
    setRuntimeFallback("Open this UI from the Tauri shell to call Rust commands.");
    return;
  }

  const [snapshot, defaultConfig, localAddresses, openxrStatus] = await Promise.all([
    invoke("bootstrap_app_state"),
    invoke("get_default_config"),
    invoke("get_local_ip_addresses"),
    invoke("get_openxr_registration_status"),
  ]);
  setAddressOptions(localAddresses, defaultConfig.hostAddress);
  renderSnapshot(snapshot, { syncForm: true });
  renderOpenXrStatus(openxrStatus);
  currentBundleId = defaultConfig.bundleId || currentBundleId;
  runtimeIds.localAddress.value = localAddresses[0] || defaultConfig.hostAddress;
  runtimeIds.port.value = defaultConfig.port;
  runtimeIds.forceQrCode.checked = defaultConfig.forceQrCode;

  window.setInterval(() => {
    refreshSnapshot().catch(() => {});
  }, 1000);
}

runtimeIds.serverToggle.addEventListener("click", async () => {
  if (!invoke) {
    return;
  }

  serverActionInFlight = true;
  renderServerButton(currentSnapshot);

  try {
    const running = isServerRunning(currentSnapshot);
    const snapshot = running
      ? await invoke("stop_server")
      : await invoke("start_server", {
          config: {
            bundleId: currentBundleId,
            hostAddress: runtimeIds.localAddress.value,
            port: Number.parseInt(runtimeIds.port.value, 10) || 55000,
            forceQrCode: runtimeIds.forceQrCode.checked,
          },
        });

    renderSnapshot(snapshot, { syncForm: true });
  } catch (error) {
    runtimeIds.detail.textContent = String(error);
  } finally {
    serverActionInFlight = false;
    renderServerButton(currentSnapshot);
  }
});

runtimeIds.openxrAction.addEventListener("click", async () => {
  if (!invoke) {
    return;
  }

  openXrActionInFlight = true;
  renderOpenXrButtons(currentOpenXrStatus);

  try {
    const status = await invoke("register_openxr_runtime_and_layer");
    renderOpenXrStatus(status);
  } catch (error) {
    runtimeIds.openxrActionNote.textContent = String(error);
    try {
      const status = await invoke("get_openxr_registration_status");
      renderOpenXrStatus(status);
      runtimeIds.openxrActionNote.textContent = `${String(error)} ${
        status.layerIsRegistered
          ? "The layer registration was refreshed."
          : ""
      }`.trim();
    } catch {
      renderOpenXrButtons(currentOpenXrStatus);
    }
  } finally {
    openXrActionInFlight = false;
    renderOpenXrButtons(currentOpenXrStatus);
  }
});

runtimeIds.openxrDeregisterLayer.addEventListener("click", async () => {
  if (!invoke) {
    return;
  }

  openXrActionInFlight = true;
  renderOpenXrButtons(currentOpenXrStatus);

  try {
    const status = await invoke("deregister_openxr_layer");
    renderOpenXrStatus(status);
    runtimeIds.openxrActionNote.textContent =
      "The Clear XR implicit layer was removed from the OpenXR layer registry.";
  } catch (error) {
    runtimeIds.openxrActionNote.textContent = String(error);
  } finally {
    openXrActionInFlight = false;
    renderOpenXrButtons(currentOpenXrStatus);
  }
});

bootstrap().catch((error) => {
  setRuntimeFallback(String(error));
});
