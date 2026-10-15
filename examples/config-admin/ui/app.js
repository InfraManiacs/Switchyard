// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Drives the configuration API from the browser. The page is served from the
// same origin as the API, so there is nothing to configure and no CORS to
// handle.

const el = (id) => document.getElementById(id);

const ui = {
  token: el("token"),
  state: el("state"),
  routes: el("routes"),
  source: el("source"),
  edited: el("edited"),
  message: el("message"),
  probeModel: el("probe-model"),
  probeMessage: el("probe-message"),
  apply: el("apply"),
};

// The version the document on screen was read at. Sent back on apply so the
// server can refuse an edit based on a configuration someone else replaced.
let loadedVersion = null;
let loadedSource = "";
// Route ids before the last apply, so the diagram can mark what changed.
let previousRouteIds = [];

ui.token.value = sessionStorage.getItem("switchyard-admin-token") || "";
ui.token.addEventListener("input", () =>
  sessionStorage.setItem("switchyard-admin-token", ui.token.value),
);

function say(node, text, kind) {
  node.textContent = text;
  node.className = `message${kind ? ` ${kind}` : ""}`;
}

function setState(headline, detail) {
  ui.state.className = `state${detail ? " live" : ""}`;
  const strong = document.createElement("b");
  strong.textContent = headline;
  const nodes = [strong];
  if (detail) {
    const rest = document.createElement("span");
    rest.textContent = detail;
    nodes.push(rest);
  }
  ui.state.replaceChildren(...nodes);
}

async function call(path, options = {}) {
  const response = await fetch(path, {
    ...options,
    headers: { ...(options.headers || {}), authorization: `Bearer ${ui.token.value}` },
  });
  const text = await response.text();
  let body = null;
  if (text) {
    try {
      body = JSON.parse(text);
    } catch {
      body = { error: text };
    }
  }
  if (!response.ok) {
    const error = new Error(body?.error || `${response.status} ${response.statusText}`);
    error.status = response.status;
    throw error;
  }
  return body;
}

function markEdited() {
  ui.edited.textContent = ui.source.value === loadedSource ? "" : "edited";
}
ui.source.addEventListener("input", markEdited);

async function load({ highlight = [] } = {}) {
  if (!ui.token.value) {
    say(ui.message, "Enter the admin token, then load.", "warn");
    return;
  }
  // Drop whatever the last attempt said, so an old error never sits next to a
  // document that loaded.
  say(ui.message, "");
  try {
    const config = await call("/v1/config");
    loadedVersion = config.version;
    loadedSource = config.source;
    ui.source.value = config.source;
    markEdited();

    setState(
      `Version ${config.version}`,
      config.persisted ? "serving, saved to file" : "serving, held in memory",
    );

    await loadRoutes(highlight);
  } catch (error) {
    setState("Not loaded");
    say(ui.message, describe(error), "bad");
  }
}

function describe(error) {
  if (error.status === 401) {
    return "That token was not accepted. Check the value the server was started with.";
  }
  if (error.status === 404) {
    return "The configuration API is off. Start the server with an admin token to enable it.";
  }
  return error.message;
}

async function loadRoutes(highlight = []) {
  const described = await call("/v1/config/routes");
  renderRoutes(described.routes || [], highlight);
  previousRouteIds = (described.routes || []).map((route) => route.id);
}

function renderRoutes(routes, highlight) {
  if (routes.length === 0) {
    const empty = document.createElement("div");
    empty.className = "empty";
    empty.textContent =
      "No routes configured. Add one to the deployment document and apply.";
    ui.routes.replaceChildren(empty);
    return;
  }

  const fragment = document.createDocumentFragment();
  for (const route of routes) {
    fragment.append(renderRoute(route, highlight.includes(route.id)));
  }
  ui.routes.replaceChildren(fragment);

  // Offer a route to probe once there is one.
  if (!ui.probeModel.value && routes[0].id) {
    ui.probeModel.placeholder = routes[0].id;
  }
}

function renderRoute(route, changed) {
  const article = document.createElement("article");
  article.className = `route${changed ? " applied" : ""}`;

  const head = document.createElement("div");
  head.className = "route-head";

  const id = document.createElement("span");
  id.className = "route-id";
  id.textContent = route.id || route.route;
  head.append(id);

  const name = document.createElement("span");
  name.className = "route-name";
  name.textContent = route.route;
  head.append(name);

  const algorithm = document.createElement("span");
  algorithm.className = "algorithm";
  algorithm.textContent = route.algorithm || "unknown";
  head.append(algorithm);

  article.append(head);

  const targets = route.targets || [];
  if (targets.length === 0) {
    const none = document.createElement("p");
    none.className = "prompt";
    none.textContent = "No targets resolved from this route.";
    article.append(none);
    return article;
  }

  const list = document.createElement("ul");
  list.className = "targets";
  // A single-target route has nothing to switch between, so its one branch is
  // where every request goes.
  const onlyPath = targets.length === 1;

  for (const target of targets) {
    const item = document.createElement("li");
    item.className = `target${onlyPath ? " serving" : ""}`;

    const line = document.createElement("div");
    line.className = "target-line";

    const targetName = document.createElement("span");
    targetName.className = "target-name";
    targetName.textContent = target.target;
    line.append(targetName);

    const model = document.createElement("span");
    model.className = "target-model";
    model.textContent = target.model || "no model id";
    line.append(model);

    if (target.llm_client) {
      const client = document.createElement("span");
      client.className = "target-client";
      client.textContent = `via ${target.llm_client}`;
      line.append(client);
    }

    item.append(line);

    if (target.system_prompt) {
      const prompt = document.createElement("p");
      prompt.className = "prompt";
      prompt.textContent = target.system_prompt;
      item.append(prompt);
    }

    list.append(item);
  }

  article.append(list);
  return article;
}

async function apply() {
  ui.apply.disabled = true;
  say(ui.message, "Applying…");
  try {
    const result = await call("/v1/config", {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ source: ui.source.value, expected_version: loadedVersion }),
    });
    const changed = previousRouteIds;
    await load({ highlight: changed });
    say(
      ui.message,
      `Applied. Version ${result.version} is serving${
        result.persisted ? ", and was written to the config file" : ", held in memory"
      }.`,
      "ok",
    );
  } catch (error) {
    if (error.status === 409) {
      say(
        ui.message,
        `${error.message}\n\nYour edits are still here. Load again to get the current document, then reapply them.`,
        "warn",
      );
    } else {
      // The server rejects a document it cannot build and keeps serving the
      // previous one. Showing its message verbatim is the point of this page.
      say(ui.message, `Not applied. Nothing changed.\n\n${error.message}`, "bad");
    }
  } finally {
    ui.apply.disabled = false;
  }
}

async function probe() {
  const model = ui.probeModel.value.trim() || ui.probeModel.placeholder;
  if (!model) {
    say(ui.probeMessage, "Enter a route id first.", "warn");
    return;
  }
  say(ui.probeMessage, "Sending…");
  try {
    const response = await fetch("/v1/chat/completions", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ model, messages: [{ role: "user", content: "ping" }] }),
    });
    const body = await response.json();
    if (!response.ok) {
      say(ui.probeMessage, body?.error?.message || JSON.stringify(body, null, 2), "bad");
      return;
    }
    const served = body.model || "an unnamed model";
    const content = body.choices?.[0]?.message?.content || "";
    say(ui.probeMessage, `${model} was served by ${served}.\n${content}`, "ok");
  } catch (error) {
    say(ui.probeMessage, error.message, "bad");
  }
}

el("load").addEventListener("click", () => load());
el("apply").addEventListener("click", apply);
el("discard").addEventListener("click", () => {
  ui.source.value = loadedSource;
  markEdited();
  say(ui.message, "Edits discarded.");
});
el("send").addEventListener("click", probe);

if (ui.token.value) {
  load();
}
