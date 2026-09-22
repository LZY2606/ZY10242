"use strict";
const $ = (sel) => document.querySelector(sel);
const $$ = (sel) => Array.from(document.querySelectorAll(sel));

let state = null;
let selectedPop = null;
let editVertices = [];
let scatterMeta = { points: [] };

function toast(msg, ok = false) {
  const el = $("#toast");
  el.textContent = msg;
  el.className = ok ? "ok" : "";
  el.style.display = "block";
  clearTimeout(toast._t);
  toast._t = setTimeout(() => (el.style.display = "none"), ok ? 2500 : 6000);
}

async function api(path, opts) {
  const res = await fetch(path, opts);
  const text = await res.text();
  const data = text ? JSON.parse(text) : null;
  if (!res.ok) throw new Error(data && data.error ? data.error : `HTTP ${res.status}`);
  return data;
}

async function refresh() {
  state = await api("/api/state");
  renderMeta();
  renderTree();
  renderPopSelect();
  renderChannelSelects();
  await Promise.all([loadScatter(), loadHistogram(), refreshRunPickers()]);
}

function renderMeta() {
  $("#fixture-meta").textContent = `fixture ${state.fixture_version} · 事件 ${state.populations.length ? "" : ""}`;
  fillSelect($("#comp-select"), state.compensations.map(c => ({ value: c.id, label: `${c.id} ${c.label}（${c.channels.join(",")}）` })), state.current_comp_id);
  fillSelect($("#transform-select"), state.transforms.map(t => ({ value: t.id, label: `${t.id} ${t.label}` })), state.current_transform_id);
}

function fillSelect(sel, items, chosen) {
  sel.innerHTML = "";
  for (const it of items) {
    const o = document.createElement("option");
    o.value = it.value;
    o.textContent = it.label;
    if (it.value === chosen) o.selected = true;
    sel.appendChild(o);
  }
}

function runFor(popId) {
  return state.runs.find(r => r.population_id === popId) || null;
}
function popById(id) {
  return state.populations.find(p => p.id === id);
}
function fmtPct(r) {
  if (!r) return "—";
  if (r.status === "invalid") return "失效";
  if (r.percent === null || r.percent === undefined) return "不可定义（父群为空）";
  return `${r.percent.toFixed(2)}%`;
}

function renderTree() {
  const root = $("#tree");
  root.className = "tree";
  root.innerHTML = "";
  const children = new Map();
  for (const p of state.populations) {
    const key = p.parent_id || "__root__";
    if (!children.has(key)) children.set(key, []);
    children.get(key).push(p);
  }
  const build = (pop) => {
    const wrap = document.createElement("div");
    const node = document.createElement("div");
    node.className = "node" + (pop.id === selectedPop ? " selected" : "");
    const r = runFor(pop.id);
    const status = !r ? "invalid" : r.status;
    const undef = r && status !== "invalid" && r.percent === null;
    node.innerHTML = `
      <div><span class="pop-name"></span>
        <span class="badge ${status === "invalid" ? "invalid" : "ok"}">${status === "invalid" ? "失效" : "当前"}</span>
        ${undef ? '<span class="badge undef">百分比不可定义</span>' : ""}
        ${r ? `<span class="badge version">${r.run_id} · ${r.comp_id} · ${r.transform_id}</span>` : ""}
      </div>
      <div class="counts"></div>`;
    node.querySelector(".pop-name").textContent = pop.id === "ALL" ? "全部事件" : `${pop.id} · ${pop.name}`;
    const counts = node.querySelector(".counts");
    if (!r) {
      counts.textContent = "该群体没有当前运行（上游失效），旧运行可在分支差异中审计";
    } else {
      const pctText = status === "invalid" ? "旧计数不显示为当前结果" : `父 ${r.parent_count === null ? "—" : r.parent_count} · 子 ${r.event_count} · 占父 ${fmtPct(r)}`;
      counts.textContent = pctText;
      if (status === "invalid") counts.textContent += `（历史计数 ${r.event_count}）`;
      const pick = document.createElement("select");
      pick.className = "run-pick";
      pick.dataset.pop = pop.id;
      const opt0 = document.createElement("option");
      opt0.value = "";
      opt0.textContent = "查看该群体全部运行…";
      pick.appendChild(opt0);
      pick.addEventListener("change", () => {
        if (pick.value) selectRunForDiff(pop.id, pick.value);
      });
      node.appendChild(pick);
    }
    node.addEventListener("click", (e) => {
      if (e.target.tagName === "SELECT" || e.target.tagName === "OPTION") return;
      selectedPop = pop.id;
      renderTree();
      renderPopSelect();
      renderChannelSelects();
      loadScatter();
      loadHistogram();
      refreshRunPickers();
    });
    wrap.appendChild(node);
    const kids = children.get(pop.id) || [];
    if (kids.length) {
      const ul = document.createElement("ul");
      for (const k of kids) ul.appendChild(build(k));
      wrap.appendChild(ul);
    }
    return wrap;
  };
  const roots = children.get("__root__") || [];
  for (const r0 of roots) root.appendChild(build(r0));
  // 拉取每个群体的历史运行填充下拉。
  for (const p of state.populations) fillRunPicker(p.id);
}

async function fillRunPicker(popId) {
  try {
    const runs = await api(`/api/runs?population_id=${encodeURIComponent(popId)}`);
    const sel = $$(".run-pick").find(s => s.dataset.pop === popId);
    if (!sel) return;
    sel.innerHTML = "";
    for (const r of runs) {
      const o = document.createElement("option");
      o.value = r.id;
      o.textContent = `${r.id} [${r.status}] ${r.gate_version_id || "root"} ${r.comp_id}/${r.transform_id} n=${r.event_count}`;
      sel.appendChild(o);
    }
  } catch (e) { /* ignore */ }
}

function renderPopSelect() {
  const sel = $("#pop-select");
  const prev = sel.value || selectedPop;
  fillSelect(sel, state.populations.filter(p => p.active_gate).map(p => ({ value: p.id, label: `${p.id} · ${p.name}` })), prev);
  selectedPop = sel.value;
  loadEditorVertices();
}

function renderChannelSelects() {
  const items = state.channels.map(c => ({ value: c.name, label: `${c.label} (${c.name})` }));
  const pop = popById(selectedPop);
  const xc = pop && pop.active_gate ? pop.active_gate.x_channel : "FSC_A";
  const yc = pop && pop.active_gate ? pop.active_gate.y_channel : "SSC_A";
  fillSelect($("#x-select"), items, $("#x-select").value || xc);
  fillSelect($("#y-select"), items, $("#y-select").value || yc);
  fillSelect($("#hist-channel"), items, $("#hist-channel").value || "CD3");
}

function loadEditorVertices() {
  const pop = popById(selectedPop);
  editVertices = pop && pop.active_gate ? pop.active_gate.vertices.map(v => ({ ...v })) : [];
  renderVertexTable();
}

function renderVertexTable() {
  const tb = $("#vertex-table tbody");
  tb.innerHTML = "";
  editVertices.forEach((v, i) => {
    const tr = document.createElement("tr");
    tr.innerHTML = `<td>${i + 1}</td><td><input type="number" step="any" /></td><td><input type="number" step="any" /></td><td><button class="small">删除</button></td>`;
    const ins = tr.querySelectorAll("input");
    ins[0].value = v.x.toFixed(3);
    ins[1].value = v.y.toFixed(3);
    ins[0].addEventListener("input", () => { v.x = parseFloat(ins[0].value); drawScatter(); });
    ins[1].addEventListener("input", () => { v.y = parseFloat(ins[1].value); drawScatter(); });
    tr.querySelector("button").addEventListener("click", () => { editVertices.splice(i, 1); renderVertexTable(); drawScatter(); });
    tb.appendChild(tr);
  });
}

async function loadScatter() {
  if (!state) return;
  const pop = popById(selectedPop);
  const x = $("#x-select").value || "FSC_A";
  const y = $("#y-select").value || "SSC_A";
  let url = `/api/scatter?x=${encodeURIComponent(x)}&y=${encodeURIComponent(y)}&limit=1000`;
  const r = runFor(selectedPop);
  if ($("#show-current").checked && r) url += `&run_id=${encodeURIComponent(r.run_id)}`;
  scatterMeta = await api(url);
  $("#scatter-space").textContent = scatterMeta.space;
  drawScatter();
}

function drawScatter() {
  const cv = $("#scatter");
  const ctx = cv.getContext("2d");
  ctx.clearRect(0, 0, cv.width, cv.height);
  const pad = 42;
  const w = cv.width - pad - 16, h = cv.height - pad - 20;
  const xName = $("#x-select").value || "FSC_A";
  const yName = $("#y-select").value || "SSC_A";
  const toPx = (x, y) => [pad + (x / 1000) * w, cv.height - pad - (y / 1000) * h];
  // 网格与刻度
  ctx.strokeStyle = "#22304d";
  ctx.fillStyle = "#8ea2c4";
  ctx.font = "10px sans-serif";
  for (let t = 0; t <= 1000; t += 200) {
    const [gx] = toPx(t, 0);
    const [, gy] = toPx(0, t);
    ctx.beginPath(); ctx.moveTo(gx, pad - 6); ctx.lineTo(gx, cv.height - pad); ctx.stroke();
    ctx.beginPath(); ctx.moveTo(pad, gy); ctx.lineTo(pad + w, gy); ctx.stroke();
    ctx.fillText(String(t), gx - 10, cv.height - pad + 14);
    ctx.fillText(String(t), 6, gy + 3);
  }
  ctx.fillStyle = "#c4d2ea";
  ctx.fillText(xName, pad + w / 2 - 20, cv.height - 2);
  ctx.save();
  ctx.translate(14, pad + h / 2);
  ctx.rotate(-Math.PI / 2);
  ctx.fillText(yName, 0, 0);
  ctx.restore();
  // 点
  for (const p of scatterMeta.points) {
    const [px, py] = toPx(Math.max(0, Math.min(1000, p.x)), Math.max(0, Math.min(1000, p.y)));
    ctx.fillStyle = p.in_run ? "rgba(74,163,255,0.85)" : "rgba(120,140,180,0.35)";
    ctx.beginPath();
    ctx.arc(px, py, p.in_run ? 3.2 : 2.2, 0, Math.PI * 2);
    ctx.fill();
  }
  // 当前门多边形（仅匹配当前坐标轴）
  const pop = popById(selectedPop);
  if (pop && pop.active_gate && pop.active_gate.x_channel === xName && pop.active_gate.y_channel === yName) {
    drawPolygon(ctx, toPx, pop.active_gate.vertices, "rgba(124,240,166,0.9)");
  }
  if (editVertices.length && pop && pop.active_gate && pop.active_gate.x_channel === xName && pop.active_gate.y_channel === yName) {
    drawPolygon(ctx, toPx, editVertices, "rgba(255,210,122,0.95)", true);
  }
}

function drawPolygon(ctx, toPx, verts, stroke, editing) {
  if (verts.length < 2) return;
  ctx.strokeStyle = stroke;
  ctx.lineWidth = editing ? 2 : 1.5;
  ctx.setLineDash(editing ? [6, 4] : []);
  ctx.beginPath();
  verts.forEach((v, i) => {
    const [px, py] = toPx(v.x, v.y);
    if (i === 0) ctx.moveTo(px, py); else ctx.lineTo(px, py);
  });
  ctx.closePath();
  ctx.stroke();
  ctx.setLineDash([]);
  for (const v of verts) {
    const [px, py] = toPx(v.x, v.y);
    ctx.fillStyle = stroke;
    ctx.fillRect(px - 3, py - 3, 6, 6);
  }
}

async function loadHistogram() {
  if (!state) return;
  const channel = $("#hist-channel").value || "CD3";
  const bins = parseInt($("#hist-bins").value || "25", 10);
  const r = runFor(selectedPop);
  let url = `/api/histogram?channel=${encodeURIComponent(channel)}&bins=${bins}`;
  if (r) {
    url += `&run_id=${encodeURIComponent(r.run_id)}`;
    const pop = popById(selectedPop);
    if (pop && pop.parent_id) {
      const pr = runFor(pop.parent_id);
      if (pr) url += `&parent_run_id=${encodeURIComponent(pr.run_id)}`;
    }
  }
  const data = await api(url);
  $("#hist-space").textContent = `${data.space} · 作用域事件 ${data.scope_count}`;
  const cv = $("#histogram");
  const ctx = cv.getContext("2d");
  ctx.clearRect(0, 0, cv.width, cv.height);
  const pad = 38, w = cv.width - pad - 12, h = cv.height - pad - 10;
  const max = Math.max(1, ...data.total);
  const bw = w / data.bins;
  for (let i = 0; i < data.bins; i++) {
    const th = (data.total[i] / max) * h;
    const sh = (data.selected[i] / max) * h;
    const x = pad + i * bw;
    ctx.fillStyle = "rgba(120,140,180,0.35)";
    ctx.fillRect(x, cv.height - pad - th, bw - 1, th);
    ctx.fillStyle = "rgba(74,163,255,0.85)";
    ctx.fillRect(x, cv.height - pad - sh, bw - 1, sh);
  }
  ctx.strokeStyle = "#22304d";
  ctx.beginPath(); ctx.moveTo(pad, 6); ctx.lineTo(pad, cv.height - pad); ctx.lineTo(pad + w, cv.height - pad); ctx.stroke();
  ctx.fillStyle = "#9fb0cc";
  ctx.fillText(`选中/门内合计 ${data.selected.reduce((a, b) => a + b, 0)}`, pad, 16);
  ctx.fillText(channel, pad + w - 60, cv.height - 6);
}

async function saveGate() {
  const pop = popById(selectedPop);
  if (!pop || !pop.active_gate) return toast("请选择一个多边形门群体");
  if (editVertices.length < 3) return toast("多边形至少需要 3 个顶点");
  const payload = {
    population_id: selectedPop,
    note: $("#gate-note").value || "页面修正多边形",
    polygon: {
      x_channel: pop.active_gate.x_channel,
      y_channel: pop.active_gate.y_channel,
      vertices: editVertices.map(v => ({ x: Number(v.x), y: Number(v.y) })),
    },
  };
  try {
    await api("/api/gates/edit", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(payload) });
    toast("已生成新门版本并为下游节点重放", true);
    $("#gate-note").value = "";
    await refresh();
  } catch (e) { toast(e.message); }
}

// ---------- 差异 ----------

async function refreshRunPickers() {
  if (!selectedPop) return;
  const runs = await api(`/api/runs?population_id=${encodeURIComponent(selectedPop)}`);
  const opts = runs.map(r => ({ value: r.id, label: `${r.id} [${r.status}] ${r.gate_version_id || "root"} ${r.comp_id}/${r.transform_id} n=${r.event_count}` }));
  fillSelect($("#diff-a"), opts, runs.find(r => r.status === "invalid") ? runs.find(r => r.status === "invalid").id : opts[0]?.value);
  fillSelect($("#diff-b"), opts, runs.find(r => r.status === "ok") ? runs.find(r => r.status === "ok").id : opts[0]?.value);
}

function selectRunForDiff(popId, runId) {
  selectedPop = popId;
  $("#pop-select").value = popId;
  refreshRunPickers().then(() => {
    const a = $("#diff-a");
    if ([...a.options].some(o => o.value === runId)) {
      const bCur = $("#diff-b").value;
      $("#diff-b").value = bCur && bCur !== runId ? bCur : (a.value || "");
      a.value = runId;
    }
    runDiff();
  });
}

async function runDiff() {
  const a = $("#diff-a").value, b = $("#diff-b").value;
  if (!a || !b) return toast("请选择两个分支运行");
  if (a === b) return toast("请选择两个不同的运行");
  const d = await api(`/api/diff?a=${encodeURIComponent(a)}&b=${encodeURIComponent(b)}`);
  $("#diff-summary").innerHTML =
    `A=${d.a.id}（${d.a.status}，门 ${d.a.gate_version_id || "无"}，${d.a.comp_id}/${d.a.transform_id}）` +
    ` ⇄ B=${d.b.id}（${d.b.status}，门 ${d.b.gate_version_id || "无"}，${d.b.comp_id}/${d.b.transform_id}）：` +
    `<span class="tag-entered">进入 ${d.entered.length}</span>，` +
    `<span class="tag-exited">退出 ${d.exited.length}</span>，` +
    `共同 ${d.both.length}，并集 ${d.union_count}`;
  const tb = $("#diff-table tbody");
  tb.innerHTML = "";
  for (const e of d.event_details) {
    const tr = document.createElement("tr");
    const label = e.category === "entered" ? "进入（仅 B）" : e.category === "exited" ? "退出（仅 A）" : "共同";
    tr.innerHTML = `<td>${e.event_id}</td><td>${e.batch_id}</td><td class="tag-${e.category}">${label}</td>`;
    tb.appendChild(tr);
  }
}

// ---------- 画布交互与全局事件 ----------

$("#scatter").addEventListener("click", (e) => {
  const pop = popById(selectedPop);
  if (!pop || !pop.active_gate) return;
  const xName = $("#x-select").value, yName = $("#y-select").value;
  if (pop.active_gate.x_channel !== xName || pop.active_gate.y_channel !== yName) return;
  const cv = $("#scatter");
  const rect = cv.getBoundingClientRect();
  const px = ((e.clientX - rect.left) / rect.width) * cv.width;
  const py = ((e.clientY - rect.top) / rect.height) * cv.height;
  const pad = 42;
  const w = cv.width - pad - 16, h = cv.height - pad - 20;
  const x = ((px - pad) / w) * 1000;
  const y = (1 - (py - (cv.height - pad - h)) / h) * 1000;
  if (x < 0 || x > 1000 || y < 0 || y > 1000) return;
  editVertices.push({ x: Number(x.toFixed(3)), y: Number(y.toFixed(3)) });
  renderVertexTable();
  drawScatter();
});

$("#add-vertex").addEventListener("click", () => { editVertices.push({ x: 200, y: 200 }); renderVertexTable(); drawScatter(); });
$("#reset-vertices").addEventListener("click", () => { loadEditorVertices(); drawScatter(); });
$("#save-gate").addEventListener("click", saveGate);
$("#pop-select").addEventListener("change", async () => {
  selectedPop = $("#pop-select").value;
  loadEditorVertices();
  renderTree();
  await loadScatter();
  await loadHistogram();
  await refreshRunPickers();
});
for (const id of ["x-select", "y-select", "show-current"]) $(`#${id}`).addEventListener("change", loadScatter);
$("#hist-channel").addEventListener("change", loadHistogram);
$("#hist-bins").addEventListener("change", loadHistogram);
$("#diff-run").addEventListener("click", () => runDiff().catch(e => toast(e.message)));

$("#comp-apply").addEventListener("click", async () => {
  try {
    await api("/api/replay/comp", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ id: $("#comp-select").value }) });
    toast("补偿矩阵已切换，全部旧运行标记失效并完成重放", true);
    await refresh();
  } catch (e) { toast(e.message); }
});
$("#transform-apply").addEventListener("click", async () => {
  try {
    await api("/api/replay/transform", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ id: $("#transform-select").value }) });
    toast("变换版本已切换，全部旧运行标记失效并完成重放", true);
    await refresh();
  } catch (e) { toast(e.message); }
});
$("#reseed-btn").addEventListener("click", async () => {
  if (!confirm("将清空当前数据库并恢复固定 fixture，确定？")) return;
  await api("/api/reseed", { method: "POST" });
  toast("已恢复固定 fixture 并重放", true);
  await refresh();
});
$("#import-file").addEventListener("change", async (e) => {
  const file = e.target.files[0];
  if (!file) return;
  try {
    const bundle = JSON.parse(await file.text());
    const report = await api("/api/import", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(bundle) });
    renderVerify(report);
    toast(report.all_ok ? "清空重导复核：所有群体成员集合一致" : "复核发现差异，请查看门系树下方报告", report.all_ok);
    await refresh();
  } catch (err) { toast(err.message); }
  e.target.value = "";
});

function renderVerify(r) {
  const el = $("#verify");
  el.innerHTML = "";
  const h = document.createElement("div");
  h.className = "badge " + (r.all_ok ? "ok" : "invalid");
  h.textContent = r.all_ok ? "重导复核全部一致" : "重导复核存在差异";
  el.appendChild(h);
  for (const c of r.checks) {
    const div = document.createElement("div");
    div.style.marginTop = "6px";
    div.textContent = `${c.population_id}: 期望 ${c.expected_count} / 重放 ${c.replayed_count} ${c.match_ok ? "✓" : "✗ 仅导出:" + c.only_in_export.join(",") + " 仅重放:" + c.only_in_replay.join(",")}`;
    el.appendChild(div);
  }
}

refresh().catch(e => toast(e.message));
