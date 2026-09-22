"use strict";
const $ = (s) => document.querySelector(s);
const $$ = (s) => Array.from(document.querySelectorAll(s));

let state = null;
let scatter = [];
let histogram = null;
const GATE_COLORS = ["#4fa3ff", "#ffb84f", "#54d486", "#e26d8c", "#b08bff", "#4fd0d0"];

function toast(msg, bad) {
  const t = $("#toast");
  t.textContent = msg;
  t.style.borderColor = bad ? "#c0405a" : "#3a527e";
  t.style.display = "block";
  clearTimeout(t._h);
  t._h = setTimeout(() => (t.style.display = "none"), 4200);
}

async function api(path, opts) {
  const res = await fetch(path, opts);
  const text = await res.text();
  let body;
  try { body = text ? JSON.parse(text) : null; } catch { body = text; }
  if (!res.ok) {
    const msg = body && body.error ? body.error : `HTTP ${res.status}`;
    throw new Error(msg);
  }
  return body;
}

const post = (path, body) =>
  api(path, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: typeof body === "string" ? body : JSON.stringify(body),
  });

async function loadState() {
  state = await api("/api/state");
  renderMeta();
  renderTree();
  renderRunHistory();
  populateSelectors();
  await loadProjection();
  await loadHist();
  await loadVersions();
}

function activeRunsByGate() {
  const m = new Map();
  for (const r of state.runs) {
    if (r.info.status === "active") m.set(r.info.gate_id, r);
  }
  return m;
}
function runsByGate() {
  const m = new Map();
  for (const r of state.runs) {
    if (!m.has(r.info.gate_id)) m.set(r.info.gate_id, []);
    m.get(r.info.gate_id).push(r);
  }
  return m;
}

function renderMeta() {
  $("#event-meta").textContent = `事件总数：${state.event_count}`;
  $("#batch-meta").textContent = `仪器批次：${state.batches.join(", ")}`;
  $("#active-comp").textContent = state.active_compensation;
  $("#active-trans").textContent = state.active_transform;
}

function pctText(run) {
  if (run.status !== "active") return null;
  if (run.parent_count === null || run.parent_count === undefined) return null;
  if (run.parent_count === 0) {
    return '<span class="pct-undefined">%父 = 不可定义（父群体为空）</span>';
  }
  return `%父 = ${run.percent_of_parent.toFixed(2)}%`;
}

function gateParents() {
  const m = new Map();
  for (const r of state.runs) {
    if (r.parent_gate && !m.has(r.gate_id)) m.set(r.gate_id, r.parent_gate);
  }
  return m;
}

function renderTree() {
  const byGate = activeRunsByGate();
  const parents = gateParents();
  const root = $("#tree");
  root.innerHTML = "";

  const children = new Map();
  for (const [child, parent] of parents) {
    if (!children.has(parent)) children.set(parent, []);
    children.get(parent).push(child);
  }
  const gateIds = new Set();
  for (const r of state.runs) gateIds.add(r.gate_id);

  function node(gateId, depth) {
    const run = byGate.get(gateId);
    const wrap = document.createElement("div");
    wrap.className = "tree-node";
    wrap.style.marginLeft = `${depth * 14}px`;
    if (!run) {
      wrap.innerHTML = `<span class="name">${gateId}</span>
        <span class="badge invalidated">无活动运行</span>`;
    } else {
      const pc = run.parent_gate
        ? (run.parent_count === 0
            ? '<span class="badge undefined">父=0 → 百分比不可定义</span>'
            : `<span class="muted">父群体 ${run.parent_count}</span>`)
        : '<span class="muted">根群体</span>';
      wrap.innerHTML = `
        <div><span class="name">${run.gate_label}</span>
          <span class="muted">(${run.gate_id})</span>
          <span class="badge active">v${run.gate_version} · active</span></div>
        <div class="count-line">事件数 <b>${run.count}</b> · ${pc}</div>
        <div class="count-line">${pctText(run) ?? ""}</div>
        <div class="muted">运行 ${run.id} · 补偿 ${run.compensation_version} · 变换 ${run.transform_version}</div>
        ${run.verify_ok === false ? '<div class="bad">复核失败：' + (run.verify_detail || "") + "</div>" : ""}
        ${run.verify_ok === true ? '<div class="ok">重放复核一致</div>' : ""}
      `;
    }
    root.appendChild(wrap);
    for (const c of (children.get(gateId) || []).slice().sort()) node(c, depth + 1);
  }
  const parentSet = new Set(parents.values());
  const roots = [...gateIds].filter((g) => !parents.has(g)).sort();
  for (const g of roots) node(g, 0);
}

function renderRunHistory() {
  const box = $("#run-history");
  box.innerHTML = "";
  const groups = runsByGate();
  for (const [gate, runs] of [...groups.entries()].sort()) {
    const head = document.createElement("div");
    head.className = "muted";
    head.style.marginTop = "6px";
    head.textContent = `${gate}（${runs.length} 次运行）`;
    box.appendChild(head);
    for (const r of runs.slice().sort((a, b) => a.created_at.localeCompare(b.created_at))) {
      const row = document.createElement("div");
      row.className = `history-row ${r.status}`;
      let countText;
      if (r.status === "invalidated") {
        countText = '<span class="bad">计数已失效（父门被修改，不得显示旧数）</span>';
      } else if (r.parent_count === 0) {
        countText = `count=${r.count} · %父=<span class="pct-undefined">不可定义</span>`;
      } else if (r.percent_of_parent === null || r.percent_of_parent === undefined) {
        countText = `count=${r.count ?? "—"}`;
      } else {
        countText = `count=${r.count} · %父=${r.percent_of_parent.toFixed(2)}%`;
      }
      row.innerHTML = `v${r.gate_version} · ${r.status} · ${countText}
        <span class="muted">${r.id}</span>`;
      box.appendChild(row);
    }
  }
}

function fillSelect(sel, values, selected) {
  sel.innerHTML = "";
  for (const v of values) {
    const o = document.createElement("option");
    o.value = typeof v === "string" ? v : v.value;
    o.textContent = typeof v === "string" ? v : v.label;
    sel.appendChild(o);
  }
  if (selected) sel.value = selected;
}

function populateSelectors() {
  const ch = state.channels;
  fillSelect($("#sel-x"), ch, $("#sel-x").value || "FSC");
  fillSelect($("#sel-y"), ch, $("#sel-y").value || "SSC");
  fillSelect($("#sel-hist"), ch, $("#sel-hist").value || "CD4");
  const active = activeRunsByGate();
  const gateOpts = [...active.values()].map((r) => ({ value: r.gate_id, label: `${r.gate_label} (${r.gate_id})` }));
  fillSelect($("#sel-gate"), gateOpts, $("#sel-gate").value || "lymph");
  fillSelect($("#sel-edit-gate"), gateOpts, $("#sel-edit-gate").value);
  fillSelect($("#new-x"), ch, "CD4");
  fillSelect($("#new-y"), ch, "CD8");
  const parentOpts = [{ value: "", label: "（根群体）" }, ...gateOpts];
  fillSelect($("#new-parent"), parentOpts, "");

  const runOpts = state.runs
    .slice()
    .sort((a, b) => a.created_at.localeCompare(b.created_at))
    .map((r) => ({
      value: r.id,
      label: `${r.gate_id} v${r.gate_version} ${r.status}`,
    }));
  fillSelect($("#diff-a"), runOpts, $("#diff-a").value);
  fillSelect($("#diff-b"), runOpts, $("#diff-b").value);
}

async function loadProjection() {
  const x = $("#sel-x").value;
  const y = $("#sel-y").value;
  scatter = await api(`/api/projection?x=${encodeURIComponent(x)}&y=${encodeURIComponent(y)}`);
  drawProjection();
}

function drawProjection() {
  const cv = $("#plot");
  const ctx = cv.getContext("2d");
  const W = cv.width, H = cv.height, M = 42;
  const x = $("#sel-x").value, y = $("#sel-y").value;
  ctx.clearRect(0, 0, W, H);
  if (!scatter.length) return;
  const xMin = Math.min(...scatter.map((p) => p.x));
  const xMax = Math.max(...scatter.map((p) => p.x));
  const yMin = Math.min(...scatter.map((p) => p.y));
  const yMax = Math.max(...scatter.map((p) => p.y));
  const sx = (v) => M + ((v - xMin) / (xMax - xMin || 1)) * (W - 2 * M);
  const sy = (v) => H - M - ((v - yMin) / (yMax - yMin || 1)) * (H - 2 * M);

  ctx.strokeStyle = "#2a3650";
  ctx.lineWidth = 1;
  ctx.beginPath();
  ctx.moveTo(M, H - M); ctx.lineTo(W - M, H - M);
  ctx.moveTo(M, H - M); ctx.lineTo(M, M);
  ctx.stroke();
  ctx.fillStyle = "#8494b3";
  ctx.font = "12px sans-serif";
  ctx.fillText(`${x} (${xMin.toFixed(0)}–${xMax.toFixed(0)})`, W / 2 - 40, H - 12);
  ctx.save();
  ctx.translate(14, H / 2);
  ctx.rotate(-Math.PI / 2);
  ctx.fillText(`${y} (${yMin.toFixed(0)}–${yMax.toFixed(0)})`, 0, 0);
  ctx.restore();

  // 成员着色：选门活动运行的成员高亮
  const active = activeRunsByGate();
  const selectedGate = $("#sel-gate").value;
  const memberSets = new Map();
  let ci = 0;
  for (const [gid, r] of active) {
    memberSets.set(gid, { set: new Set(r.members), color: GATE_COLORS[ci++ % GATE_COLORS.length] });
  }
  for (const p of scatter) {
    const m = memberSets.get(selectedGate);
    let color = "#5b6b8c";
    let radius = 1.6;
    if (m && m.set.has(p.id)) { color = m.color; radius = 2.6; }
    ctx.fillStyle = color;
    ctx.beginPath();
    ctx.arc(sx(p.x), sy(p.y), radius, 0, Math.PI * 2);
    ctx.fill();
  }

  // 叠加门多边形（仅通道匹配时）
  const legend = $("#legend");
  legend.innerHTML = "";
  let li = 0;
  for (const r of state.runs.filter((r) => r.status === "active")) {
    if (r.x_channel !== x || r.y_channel !== y) continue;
    const color = GATE_COLORS[li++ % GATE_COLORS.length];
    ctx.strokeStyle = color;
    ctx.lineWidth = r.gate_id === selectedGate ? 2.4 : 1.2;
    ctx.setLineDash(r.gate_id === selectedGate ? [] : [5, 4]);
    ctx.beginPath();
    r.vertices.forEach((v, i) => {
      const px = sx(v[0]), py = sy(v[1]);
      if (i === 0) ctx.moveTo(px, py); else ctx.lineTo(px, py);
    });
    ctx.closePath();
    ctx.stroke();
    ctx.setLineDash([]);
    const item = document.createElement("span");
    item.innerHTML = `<i style="background:${color}"></i>${r.gate_label} (${r.gate_id})`;
    legend.appendChild(item);
  }
}

async function loadHist() {
  const ch = $("#sel-hist").value;
  histogram = await api(`/api/hist?channel=${encodeURIComponent(ch)}&bins=30`);
  drawHist();
}

function drawHist() {
  const cv = $("#hist");
  const ctx = cv.getContext("2d");
  const W = cv.width, H = cv.height, M = 42;
  ctx.clearRect(0, 0, W, H);
  if (!histogram) return;
  const max = Math.max(1, ...histogram.counts);
  const bw = (W - 2 * M) / histogram.counts.length;
  histogram.counts.forEach((c, i) => {
    const h = (c / max) * (H - 2 * M);
    ctx.fillStyle = "#4fa3ff";
    ctx.fillRect(M + i * bw + 1, H - M - h, bw - 2, h);
  });
  ctx.strokeStyle = "#2a3650";
  ctx.beginPath();
  ctx.moveTo(M, H - M); ctx.lineTo(W - M, H - M);
  ctx.stroke();
  ctx.fillStyle = "#8494b3";
  ctx.fillText(`${histogram.channel}  范围 ${histogram.lo.toFixed(1)} – ${histogram.hi.toFixed(1)}`, M, 18);
}

async function loadVersions() {
  const v = await api("/api/versions");
  const compBox = $("#comp-list");
  compBox.innerHTML = "";
  for (const c of v.compensations) {
    const row = document.createElement("div");
    row.className = "vrow";
    row.innerHTML = `${c.active ? "●" : "○"} ${c.label} <span class="muted">${c.id}</span>`;
    if (!c.active) {
      const b = document.createElement("button");
      b.textContent = "切换到此补偿（全量重放）";
      b.onclick = async () => {
        try {
          state = await post("/api/switch", { compensation: c.id });
          await loadState();
          toast("补偿版本已切换，所有节点生成新运行，旧运行保留为 superseded。");
        } catch (e) { toast(e.message, true); }
      };
      row.appendChild(b);
    }
    compBox.appendChild(row);
  }
  const trBox = $("#trans-list");
  trBox.innerHTML = "";
  for (const t of v.transforms) {
    const row = document.createElement("div");
    row.className = "vrow";
    row.innerHTML = `${t.active ? "●" : "○"} ${t.label} <span class="muted">${t.id}</span>`;
    if (!t.active) {
      const b = document.createElement("button");
      b.textContent = "切换到此变换（全量重放）";
      b.onclick = async () => {
        try {
          state = await post("/api/switch", { transform: t.id });
          await loadState();
          toast("变换版本已切换，所有节点生成新运行。");
        } catch (e) { toast(e.message, true); }
      };
      row.appendChild(b);
    }
    trBox.appendChild(row);
  }
}

async function loadEditVertices() {
  const gateId = $("#sel-edit-gate").value;
  const run = activeRunsByGate().get(gateId);
  if (!run) return;
  $("#edit-vertices").value = JSON.stringify(run.vertices);
  $("#edit-label").value = `${run.gate_label} 修正`;
}

async function showGateVersions() {
  const gateId = $("#sel-edit-gate").value;
  const versions = await api(`/api/gate-versions?gate_id=${encodeURIComponent(gateId)}`);
  const box = $("#gate-version-list");
  box.innerHTML = "";
  for (const v of versions) {
    const row = document.createElement("div");
    row.className = "vrow";
    row.innerHTML = `v${v.version} ${v.active ? "●活动" : "○历史"} — ${v.label}
      <div class="muted">x=${v.x_channel} y=${v.y_channel} ${v.parent ? "父=" + v.parent : "根"}</div>`;
    if (!v.active) {
      const b = document.createElement("button");
      b.textContent = "回放此版本（生成新分支运行）";
      b.onclick = async () => {
        try {
          state = await post("/api/gates/activate", { gate_id: gateId, gate_version: v.db_id });
          await loadState();
          toast("已激活历史门定义并重放；旧子孙运行已失效。");
        } catch (e) { toast(e.message, true); }
      };
      row.appendChild(b);
    }
    box.appendChild(row);
  }
}

async function saveVersion() {
  const gateId = $("#sel-edit-gate").value;
  const run = activeRunsByGate().get(gateId);
  let vertices;
  try {
    vertices = JSON.parse($("#edit-vertices").value);
  } catch {
    return toast("顶点 JSON 解析失败", true);
  }
  try {
    state = await post("/api/gates/version", {
      gate_id: gateId,
      label: $("#edit-label").value || "修正版本",
      parent: run.parent_gate,
      x_channel: run.x_channel,
      y_channel: run.y_channel,
      vertices,
    });
    await loadState();
    toast("已生成新版本：本门旧运行 superseded，下游旧子门运行 invalidated。");
  } catch (e) {
    toast(e.message, true);
  }
}

async function createNewGate() {
  let vertices;
  try {
    vertices = JSON.parse($("#new-vertices").value);
  } catch {
    return toast("顶点 JSON 解析失败", true);
  }
  try {
    state = await post("/api/gates/new", {
      id: $("#new-id").value.trim(),
      label: $("#new-label").value.trim() || $("#new-id").value.trim(),
      parent: $("#new-parent").value || null,
      x_channel: $("#new-x").value,
      y_channel: $("#new-y").value,
      vertices,
    });
    await loadState();
    toast("新门已创建并完成首次重放。");
  } catch (e) {
    toast(e.message, true);
  }
}

async function addCompensation() {
  let channels, matrix;
  try {
    channels = JSON.parse($("#comp-channels").value);
    matrix = JSON.parse($("#comp-matrix").value);
  } catch {
    return toast("补偿 JSON 解析失败", true);
  }
  try {
    await post("/api/compensations", {
      label: $("#comp-label").value || "补偿新版本",
      channels,
      matrix,
      activate: $("#comp-activate").checked,
    });
    await loadState();
    toast("补偿版本已创建" + ($("#comp-activate").checked ? "并切换、全量重放。" : "。"));
  } catch (e) {
    toast(e.message, true);
  }
}

async function addTransform() {
  let params;
  try {
    params = JSON.parse($("#trans-params").value);
  } catch {
    return toast("变换 JSON 解析失败", true);
  }
  try {
    await post("/api/transforms", {
      label: $("#trans-label").value || "变换新版本",
      params,
      activate: $("#trans-activate").checked,
    });
    await loadState();
    toast("变换版本已创建" + ($("#trans-activate").checked ? "并切换、全量重放。" : "。"));
  } catch (e) {
    toast(e.message, true);
  }
}

async function showDiff() {
  try {
    const d = await api(`/api/diff?a=${encodeURIComponent($("#diff-a").value)}&b=${encodeURIComponent($("#diff-b").value)}`);
    const box = $("#diff-result");
    box.innerHTML = `
      <div>仅在 A：<b>${d.only_in_a.length}</b>；仅在 B：<b>${d.only_in_b.length}</b>；共有：${d.in_both}</div>
      <div class="muted">A 独有：${d.only_in_a.slice(0, 60).join(", ") || "（无）"}</div>
      <div class="muted">B 独有：${d.only_in_b.slice(0, 60).join(", ") || "（无）"}</div>`;
  } catch (e) {
    toast(e.message, true);
  }
}

function download(name, text) {
  const blob = new Blob([text], { type: "application/json" });
  const a = document.createElement("a");
  a.href = URL.createObjectURL(blob);
  a.download = name;
  a.click();
  URL.revokeObjectURL(a.href);
}

function wire() {
  $("#sel-x").onchange = loadProjection;
  $("#sel-y").onchange = loadProjection;
  $("#sel-gate").onchange = drawProjection;
  $("#sel-hist").onchange = loadHist;
  $("#btn-load-version").onclick = loadEditVertices;
  $("#btn-history").onclick = showGateVersions;
  $("#btn-save-version").onclick = saveVersion;
  $("#btn-new-gate").onclick = createNewGate;
  $("#btn-comp-add").onclick = addCompensation;
  $("#btn-trans-add").onclick = addTransform;
  $("#btn-diff").onclick = showDiff;
  $("#sel-edit-gate").onchange = loadEditVertices;

  $("#btn-export").onclick = async () => {
    const bundle = await api("/api/export");
    download("flow-gate-runs.json", JSON.stringify(bundle, null, 2));
    toast("运行记录已导出。");
  };
  $("#btn-verify").onclick = async () => {
    const r = await api("/api/verify");
    const box = $("#io-result");
    box.innerHTML = `<div class="${r.failures.length ? "bad" : "ok"}">
      复核 active 运行 ${r.checked} 条，通过 ${r.passed} 条。
      ${r.failures.length ? "<br>" + r.failures.join("<br>") : "全部一致。"}</div>`;
    await loadState();
  };
  $("#btn-reseed").onclick = async () => {
    state = await post("/api/reseed", {});
    await loadState();
    toast("数据库已清空并重新播种 fixture。");
  };
  $("#import-file").onchange = async (ev) => {
    const file = ev.target.files[0];
    if (!file) return;
    const text = await file.text();
    try {
      const r = await post("/api/import", text);
      const box = $("#io-result");
      box.innerHTML = `<div class="${r.failures.length ? "bad" : "ok"}">
        已清空数据库并导入：运行 ${r.imported} 条，重放复核通过 ${r.passed} 条。
        ${r.failures.length ? "<br>" + r.failures.slice(0, 10).join("<br>") : ""}</div>`;
      await loadState();
      toast("清空后导入并完成复核。");
    } catch (e) {
      toast(e.message, true);
    }
  };
}

wire();
loadState().catch((e) => toast("初始化失败：" + e.message, true));
