# 流式门谱台（Flow Gate Station）

本地运行的流式细胞术门控工作台。每个**群体定义（多边形门）、父群体关系、补偿矩阵、
仪器变换**都是只追加、可切换、可重放的版本快照。修改上游门或变换后，系统为受影响的
下游节点生成新运行；原门系定义与旧计数完整保留，旧子门运行明确进入 `invalidated`
状态，绝不继续显示旧数。

技术栈：Rust + Axum + SQLite（`rusqlite` bundled，无需系统 SQLite）+ 原生 Web UI。

## 安装与演示

```bash
cargo fetch --locked
cargo test --locked && cargo run --locked -- --listen 127.0.0.1:5582
# 浏览器打开
open http://127.0.0.1:5582
```

页面标题为「流式门谱台」。首次启动若数据库为空，自动播种固定 fixture
（`flow-gate-station.db`，可通过 `--db PATH` 更换）。

## 功能

- **双通道投影**：任选两个通道，叠加当前活动多边形门；活动门成员高亮。
- **直方分布**：任一通道（活动补偿+变换后的口径）30 档直方图。
- **门系树**：父子结构、每个活动群体的事件数、占父群体百分比与所用版本。
- **修正多边形门**：提交新版本 → 本门旧运行变 `superseded`，全部下游旧运行变
  `invalidated`（计数清空），并在一个事务内按拓扑序重放受影响子树。
- **切换补偿 / 变换版本**：所有旧运行 `superseded`，全量重放；可随时切回精确还原。
- **历史门版本回放**：激活任一历史定义形成新分支。
- **分支进出差异**：任选两个运行，按**事件身份**给出仅在 A、仅在 B、共有。
- **运行记录导出 / 清空重导入复核**：导出整个版本+运行 bundle；导入会先清空数据库，
  再恢复原始记录并对每条 active 运行独立重算核对（`/api/verify`）。
- 所有变更接口都由单一 SQLite 互斥串行化；并发修改父门后，旧子门运行同样确定失效。

## 数据口径（重要）

1. **事件**：固定 5 个通道，顺序固定为 `FSC, SSC, CD45, CD4, CD8`，原始强度 0–1000，
   按事件 id 存储（200 条 LCG 背景事件 + 10 条坐标精确构造的边界 QC 事件）。
2. **补偿**：矩阵显式带通道名（行=输出通道，列=源通道）。应用时**逐元素按名字查表**，
   与数据列的物理位置无关。矩阵声明的通道集合必须与数据通道集合**完全一致**：
   缺通道、多通道一律拒绝（HTTP 422），不会按位置继续做矩阵乘法。
   fixture 内置一次补偿通道顺序变化：`comp-v2` 以**反序**声明通道并带 CD8→CD4
   2% 溢出项，用于演示“顺序变化但按名字正确重放”。
3. **变换**：版本化的逐通道函数。`linear: scale*x+offset`；
   `log10: scale*log10(max(x-offset,1e-9))+bias`。fixture 提供线性 v1 与荧光通道
   log10 v2。门几何始终在“补偿后 + 变换后”的坐标空间求值。
4. **计数与百分比**：`count = 父群体（根门为全体事件）中落在多边形内的事件数`；
   `percent_of_parent = 100 * count / parent_count`。**父群体为空（parent_count=0）时
   百分比是不可定义（页面显示“不可定义”，JSON 为 `null`），不是 0**。

## 半开几何规则（边界不双计）

统一半开多边形规则（绕数法，端点半开，与标准光栅化约定一致）：

- 从查询点水平向右的射线，边 `a→b` 被计为“向上穿过”当且仅当 `a.y <= p.y < b.y`；
  向下对称。交点在射线左/右由方向叉积的符号直接判定，**不计算交点坐标**。
- 因此两个多边形若共享一条边，它们对该边上任意点的判定严格互补：恰好一个计入。
  fixture 中 `cd4` 与 `cd8` 在 `CD4=500` 共享一条竖边（两多边形中遍历方向相反），
  边上的 QC 事件 `qc-shared-1/2` 只进入 `cd8`，任何事件都不会被两门同时计数。
- 半开矩形 `lymph`：底边与左边上的事件计入，顶边与右边上的事件计出
  （`qc-edge-bottom/left/top/right`）。
- 若两个同层多边形的边界相互接触、但共享边端点对不上（数值漂移导致几何歧义），
  保存会被拒绝（`shared-edge` 校验），而不是带着歧义继续计数。

## 运行状态

| 状态 | 含义 | 计数 |
| --- | --- | --- |
| `active` | 当前活动定义下的最新运行 | 有 |
| `superseded` | 同一门节点被新版本/新补偿/新变换取代 | 保留旧值供审计 |
| `invalidated` | 祖先门被修改，旧子门运行无法再解释 | **置空（NULL），UI 不显示旧数** |

## 运行记录导出与复核

- `GET /api/export`：导出事件、补偿/变换/门/门版本、全部运行（含失效记录）的 JSON bundle。
- `POST /api/import`（body 为 bundle）：清空数据库 → 恢复记录 → 对每条 active 运行按其
  记录的定义/补偿/变换快照与祖先链独立重算，逐条核对事件身份集合，返回
  `imported/passed/failures`。
- `GET /api/verify`：在不清空数据的前提下对当前库做同样的重算复核。
- 页面「清空并重播种 fixture」可一键回到出厂状态后再复核。

## 主要 HTTP 接口

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/api/state` | 事件数、批次、活动版本、全部运行（含成员 id） |
| GET | `/api/projection?x&y` | 变换后双通道坐标 |
| GET | `/api/hist?channel&bins` | 直方图 |
| GET | `/api/runs` | 全部运行（含失效/被取代） |
| GET | `/api/gate-versions?gate_id` | 某门的全部历史定义版本 |
| GET | `/api/versions` | 补偿/变换版本清单 |
| GET | `/api/diff?a&b` | 两个运行按事件身份的进出差异 |
| POST | `/api/gates/version` | 为门提交新多边形版本并重放受影响子树 |
| POST | `/api/gates/activate` | 激活某门的历史定义版本 |
| POST | `/api/gates/new` | 新建门 |
| POST | `/api/switch` | 切换活动补偿/变换并全量重放 |
| POST | `/api/compensations`, `/api/transforms` | 新建版本（通道不匹配返回 422） |
| GET | `/api/export` · POST | `/api/import` · GET `/api/verify` · POST `/api/reseed` |

## 测试

- 单元测试：补偿按名应用/缺通道拒绝、半开几何与共享边划分、近重合共享边拒绝。
- 验收测试（`tests/acceptance.rs`，共 12 项）：
  边界事件不双计、空父百分比为 `null`、父门修改后旧子门运行 `invalidated` 且计数清空、
  补偿通道缺/多拒绝、反序补偿按名重放且可还原、log10 变换切换与分支差异、
  导出→清空→重导入→重算复核一致、历史门版本回放、并发修改父门无残留活动旧运行等。

```bash
cargo test --locked
```

## 项目结构

```
src/domain.rs       类型：通道、变换、点/多边形、运行状态
src/geometry.rs     半开绕数几何 + 共享边一致性校验
src/compensation.rs 补偿矩阵：按通道名应用、缺通道拒绝
src/fixture.rs      固定 LCG 事件集、边界 QC 事件、门系与版本
src/storage.rs      SQLite schema 与版本只追加存储
src/engine.rs       补偿→变换管线、拓扑重放、失效/取代、差异、导出/导入/复核
src/web.rs          Axum 路由与 JSON API
src/static/         单页 Web UI
tests/acceptance.rs 验收测试
```
