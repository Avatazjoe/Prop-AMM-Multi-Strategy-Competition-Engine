import { useState, useRef, useEffect, useCallback } from "react";

// ─── Static leaderboard seed data ────────────────────────────────────────────
const LEADERBOARD_DATA = [
  { rank: 1, author: "@volsurfer_eth",  strategy: "GBM-Gated Spread v4",      edge: 612.44, attempts: 14, model: "None" },
  { rank: 2, author: "@bytecode_quant", strategy: "shadow_price_arb",          edge: 608.91, attempts: 31, model: "None" },
  { rank: 3, author: "@meridian_mm",    strategy: "EpochAware EWMA",           edge: 605.17, attempts: 8,  model: "claude-sonnet-4" },
  { rank: 4, author: "@flowcapture22",  strategy: "equimarginal_v2",            edge: 601.03, attempts: 47, model: "None" },
  { rank: 5, author: "@arb_sentinel",   strategy: "CapWeightedFee",            edge: 598.82, attempts: 22, model: "GPT-5" },
  { rank: 6, author: "@retrade_labs",   strategy: "vol_regime_switch",         edge: 594.40, attempts: 9,  model: "None" },
  { rank: 7, author: "@0xfrokk",        strategy: "tick_adaptive_r7",          edge: 591.29, attempts: 63, model: "None" },
  { rank: 8, author: "@basedquant",     strategy: "LameSpreader_deprecated",   edge: 587.55, attempts: 5,  model: "None" },
  { rank: 9, author: "@pnl_maximoor",   strategy: "poisson_flow_v1",           edge: 583.11, attempts: 19, model: "claude-opus-4" },
  { rank: 10,author: "@mm_enjoyor",     strategy: "StaticPlus30",              edge: 571.22, attempts: 3,  model: "None" },
];

const STARTER_CODE = `use prop_amm_submission_sdk::{
    AfterSwapContext, EpochContext, Storage, SwapContext,
    bps_to_wad, clamp_fee, cpamm_output_wad, read_f64,
    read_u64, write_f64, write_u64, WAD,
};

pub const NAME: &str = "My Strategy";
pub const MODEL_USED: &str = "None";

// Storage slots (each = 8 bytes)
const S_BID_FEE:   usize = 0;
const S_ASK_FEE:   usize = 1;
const S_VOL_EST:   usize = 2;
const S_LAST_SPOT: usize = 3;

pub fn compute_swap(ctx: &SwapContext) -> u64 {
    let fee = read_u64(&ctx.storage, S_BID_FEE)
        .max(bps_to_wad(5));

    let (reserve_in, reserve_out) = if ctx.is_buy {
        (ctx.reserve_y, ctx.reserve_x)
    } else {
        (ctx.reserve_x, ctx.reserve_y)
    };

    cpamm_output_wad(ctx.input_amount, reserve_in, reserve_out, fee)
}

pub fn after_swap(ctx: &AfterSwapContext, storage: &mut Storage) {
    // Update vol estimate from price move
    let spot = ctx.spot_price();
    let last = read_f64(storage, S_LAST_SPOT);
    let vol  = read_f64(storage, S_VOL_EST);

    let new_vol = if last > 0.0 {
        let ret = (spot / last).ln().abs();
        0.05 * ret + 0.95 * vol
    } else { 0.003 };

    // Scale fee with vol: base 30 bps + vol premium
    let premium_bps = (new_vol * 1_000_000.0).min(200.0) as u64;
    let fee = clamp_fee(bps_to_wad(30 + premium_bps));

    write_u64(storage, S_BID_FEE, fee);
    write_u64(storage, S_ASK_FEE, fee);
    write_f64(storage, S_VOL_EST, new_vol);
    write_f64(storage, S_LAST_SPOT, spot);
}

pub fn on_epoch_boundary(ctx: &EpochContext, storage: &mut Storage) {
    // Regress vol estimate at epoch boundary
    let vol = read_f64(storage, S_VOL_EST);
    write_f64(storage, S_VOL_EST, vol * 0.5 + 0.003 * 0.5);
}`;

// ─── Pipeline stages ──────────────────────────────────────────────────────────
const STAGES = [
  { id: "upload",   label: "UPLOAD",   desc: "Source received" },
  { id: "compile",  label: "COMPILE",  desc: "Rust → native + BPF" },
  { id: "validate", label: "VALIDATE", desc: "Monotone · Concave · Symbols" },
  { id: "simulate", label: "SIMULATE", desc: "1,000 × 10,000 steps" },
  { id: "score",    label: "SCORE",    desc: "Edge vs normalizer" },
];

const sleep = ms => new Promise(r => setTimeout(r, ms));

// ─── Component ────────────────────────────────────────────────────────────────
export default function PropAMMSubmit() {
  const [tab, setTab] = useState("submit"); // submit | leaderboard
  const [code, setCode] = useState("");
  const [handle, setHandle] = useState("@avataz_joe");
  const [fileName, setFileName] = useState(null);
  const [dragging, setDragging] = useState(false);
  const [stage, setStage] = useState(null); // null | stage id | "done" | "error"
  const [stageIdx, setStageIdx] = useState(-1);
  const [stageLog, setStageLog] = useState([]);
  const [result, setResult] = useState(null);
  const [error, setError] = useState(null);
  const [lineCount, setLineCount] = useState(0);
  const fileRef = useRef();
  const logRef = useRef();
  const textareaRef = useRef();

  useEffect(() => {
    setLineCount(code.split("\n").length);
  }, [code]);

  useEffect(() => {
    if (logRef.current) {
      logRef.current.scrollTop = logRef.current.scrollHeight;
    }
  }, [stageLog]);

  const handleFile = useCallback((file) => {
    if (!file) return;
    if (!file.name.endsWith(".rs")) {
      setError("Only .rs source files accepted.");
      return;
    }
    setFileName(file.name);
    const reader = new FileReader();
    reader.onload = e => setCode(e.target.result);
    reader.readAsText(file);
    setError(null);
  }, []);

  const onDrop = useCallback(e => {
    e.preventDefault();
    setDragging(false);
    handleFile(e.dataTransfer.files[0]);
  }, [handleFile]);

  const pushLog = (line) => setStageLog(prev => [...prev, line]);

  const runSubmission = async () => {
    if (!handle.trim()) { setError("X / Twitter handle required."); return; }
    if (!code.trim())   { setError("No strategy code provided."); return; }
    if (!code.includes("compute_swap")) { setError("Strategy must implement compute_swap."); return; }

    setError(null);
    setResult(null);
    setStageLog([]);
    setStage("running");

    const logs = {
      upload: [
        `→ Received ${fileName || "lib.rs"} (${(new Blob([code]).size / 1024).toFixed(1)} KB)`,
        "→ Author: " + handle,
        "→ Queued for compilation",
      ],
      compile: [
        "→ rustc 1.83.0 (90b35a623 2024-11-26)",
        "→ Compiling prop_amm_submission_sdk v0.1.0",
        "→ Compiling strategy (native cdylib)...",
        "→ Compiling strategy (BPF target)...",
        "✓ Build succeeded in 4.2s",
      ],
      validate: [
        "→ Checking __prop_amm_compute_swap … found",
        "→ Checking __prop_amm_after_swap  … found",
        "→ Checking __prop_amm_get_name    … found",
        "→ Monotonicity: testing 50 input points … ✓",
        "→ Concavity: testing marginal outputs  … ✓",
        "→ Native / BPF parity: Δ < 1e-9       … ✓",
        "✓ Validation passed",
      ],
      simulate: [
        "→ Workers: 8 threads",
        "→ Seed range: server holdout [classified]",
        "→ Sim   100 / 1000  (σ=0.31%, λ=0.91, norm_fee=47bps)",
        "→ Sim   250 / 1000  (σ=0.18%, λ=0.64, norm_fee=62bps)",
        "→ Sim   500 / 1000  (σ=0.55%, λ=1.05, norm_fee=31bps)",
        "→ Sim   750 / 1000  (σ=0.09%, λ=0.78, norm_fee=78bps)",
        "→ Sim  1000 / 1000  (σ=0.42%, λ=1.18, norm_fee=44bps)",
        "✓ 1,000 simulations complete in 5.1s",
      ],
      score: [
        "→ Computing edge distribution ...",
        "→ Normalizer mean edge: 312.44",
      ],
    };

    for (let i = 0; i < STAGES.length; i++) {
      const s = STAGES[i];
      setStageIdx(i);
      setStage(s.id);
      const lines = logs[s.id];
      for (const line of lines) {
        pushLog(line);
        await sleep(180 + Math.random() * 120);
      }
      await sleep(300);
    }

    // Final score
    const edge = 480 + Math.random() * 140;
    const std  = 80 + Math.random() * 60;
    const rank = Math.floor(Math.random() * 5) + 1;
    pushLog(`→ Your strategy mean edge: ${edge.toFixed(2)}`);
    pushLog(`→ Std dev: ${std.toFixed(2)}`);
    pushLog(`→ vs Normalizer: +${(edge - 312.44).toFixed(2)}`);
    pushLog(`→ Sharpe: ${(edge / std).toFixed(3)}`);
    pushLog(`✓ Submission scored — leaderboard updated`);

    await sleep(400);
    setStage("done");
    setResult({ edge, std, rank, handle });
  };

  const reset = () => {
    setStage(null); setStageIdx(-1); setStageLog([]);
    setResult(null); setError(null); setFileName(null);
  };

  return (
    <div style={{
      minHeight: "100vh",
      background: "#080c0f",
      fontFamily: "'JetBrains Mono', 'Fira Code', 'Cascadia Code', monospace",
      color: "#c8d8e8",
      position: "relative",
      overflow: "hidden",
    }}>
      <style>{`
        @import url('https://fonts.googleapis.com/css2?family=JetBrains+Mono:wght@300;400;500;600;700&family=Syne:wght@400;600;700;800&display=swap');

        *, *::before, *::after { box-sizing: border-box; margin: 0; padding: 0; }

        ::-webkit-scrollbar { width: 4px; height: 4px; }
        ::-webkit-scrollbar-track { background: #0d1419; }
        ::-webkit-scrollbar-thumb { background: #1e3040; border-radius: 2px; }
        ::-webkit-scrollbar-thumb:hover { background: #2a4a60; }

        .grid-bg {
          position: fixed; inset: 0; pointer-events: none; z-index: 0;
          background-image:
            linear-gradient(rgba(0,180,255,0.03) 1px, transparent 1px),
            linear-gradient(90deg, rgba(0,180,255,0.03) 1px, transparent 1px);
          background-size: 40px 40px;
        }

        .scanline {
          position: fixed; inset: 0; pointer-events: none; z-index: 1;
          background: repeating-linear-gradient(
            0deg, transparent, transparent 2px, rgba(0,0,0,0.03) 2px, rgba(0,0,0,0.03) 4px
          );
        }

        .glow-amber { color: #f5a623; text-shadow: 0 0 12px rgba(245,166,35,0.5); }
        .glow-green { color: #00e676; text-shadow: 0 0 12px rgba(0,230,118,0.4); }
        .glow-blue  { color: #29b6f6; text-shadow: 0 0 12px rgba(41,182,246,0.4); }
        .glow-red   { color: #ff5252; text-shadow: 0 0 12px rgba(255,82,82,0.5); }

        .nav-link {
          color: #607080; text-decoration: none; font-size: 11px;
          letter-spacing: 0.12em; text-transform: uppercase; padding: 6px 0;
          border-bottom: 2px solid transparent; transition: all 0.15s;
          cursor: pointer; background: none; border-top: none; border-left: none; border-right: none;
          font-family: inherit;
        }
        .nav-link:hover { color: #c8d8e8; }
        .nav-link.active { color: #f5a623; border-bottom-color: #f5a623; }

        .code-area {
          background: #050810;
          border: 1px solid #1a2a38;
          border-radius: 4px;
          color: #a8c4d8;
          font-family: 'JetBrains Mono', monospace;
          font-size: 12px;
          line-height: 1.65;
          resize: none;
          outline: none;
          transition: border-color 0.2s;
          tab-size: 4;
          width: 100%;
          padding: 16px 16px 16px 52px;
        }
        .code-area:focus { border-color: #2a4a6a; }

        .input-field {
          background: #050810;
          border: 1px solid #1a2a38;
          border-radius: 4px;
          color: #c8d8e8;
          font-family: 'JetBrains Mono', monospace;
          font-size: 13px;
          padding: 10px 14px;
          outline: none;
          width: 100%;
          transition: border-color 0.2s;
        }
        .input-field:focus { border-color: #f5a623; }
        .input-field::placeholder { color: #2a3a4a; }

        .btn-primary {
          background: #f5a623;
          color: #080c0f;
          border: none;
          border-radius: 4px;
          font-family: 'JetBrains Mono', monospace;
          font-size: 12px;
          font-weight: 700;
          letter-spacing: 0.15em;
          text-transform: uppercase;
          padding: 12px 32px;
          cursor: pointer;
          transition: all 0.15s;
        }
        .btn-primary:hover { background: #ffc043; transform: translateY(-1px); box-shadow: 0 4px 20px rgba(245,166,35,0.3); }
        .btn-primary:active { transform: translateY(0); }
        .btn-primary:disabled { background: #2a3a4a; color: #4a5a6a; cursor: not-allowed; transform: none; box-shadow: none; }

        .btn-ghost {
          background: transparent;
          color: #607080;
          border: 1px solid #1a2a38;
          border-radius: 4px;
          font-family: 'JetBrains Mono', monospace;
          font-size: 11px;
          letter-spacing: 0.1em;
          text-transform: uppercase;
          padding: 8px 18px;
          cursor: pointer;
          transition: all 0.15s;
        }
        .btn-ghost:hover { color: #c8d8e8; border-color: #2a4a6a; }

        .stage-pill {
          display: flex; align-items: center; gap: 8px;
          padding: 6px 12px; border-radius: 3px;
          font-size: 10px; letter-spacing: 0.12em;
          border: 1px solid #1a2a38;
          background: #080c0f;
          transition: all 0.3s;
        }
        .stage-pill.active {
          border-color: #f5a623;
          background: rgba(245,166,35,0.06);
          color: #f5a623;
        }
        .stage-pill.done {
          border-color: #00e676;
          background: rgba(0,230,118,0.04);
          color: #00e676;
        }
        .stage-pill.pending { color: #2a4060; }

        .log-line { padding: 1px 0; font-size: 11px; line-height: 1.6; }
        .log-line.ok { color: #00e676; }
        .log-line.info { color: #607080; }
        .log-line.arrow { color: #4a6a8a; }

        .drop-zone {
          border: 1px dashed #1a3050;
          border-radius: 4px;
          padding: 18px;
          text-align: center;
          cursor: pointer;
          transition: all 0.2s;
          font-size: 11px; color: #3a5570; letter-spacing: 0.08em;
        }
        .drop-zone:hover, .drop-zone.drag { border-color: #f5a623; color: #f5a623; background: rgba(245,166,35,0.03); }

        .lb-row {
          display: grid;
          grid-template-columns: 36px 1fr 1fr 90px 60px;
          gap: 0; align-items: center;
          padding: 9px 16px;
          border-bottom: 1px solid #0e1a22;
          font-size: 12px;
          transition: background 0.15s;
        }
        .lb-row:hover { background: #0d1820; }
        .lb-row.header {
          font-size: 9px; letter-spacing: 0.14em; text-transform: uppercase;
          color: #2a4a6a; border-bottom: 1px solid #1a2a38;
          padding: 8px 16px;
        }

        .medal-1 { color: #ffd700; }
        .medal-2 { color: #c0c0c0; }
        .medal-3 { color: #cd7f32; }

        .result-card {
          background: #050810;
          border: 1px solid #00e676;
          border-radius: 6px;
          padding: 28px 32px;
          text-align: center;
          animation: fadeUp 0.4s ease;
        }
        @keyframes fadeUp {
          from { opacity: 0; transform: translateY(12px); }
          to   { opacity: 1; transform: translateY(0); }
        }

        .ticker-char {
          display: inline-block;
          animation: blink 1s step-end infinite;
        }
        @keyframes blink { 50% { opacity: 0; } }

        .section-label {
          font-size: 9px; letter-spacing: 0.2em; text-transform: uppercase;
          color: #2a4a6a; margin-bottom: 8px;
        }

        .line-numbers {
          position: absolute; left: 0; top: 0; width: 44px;
          padding: 16px 8px 16px 0;
          text-align: right;
          font-size: 12px; line-height: 1.65;
          color: #1e3040; pointer-events: none;
          user-select: none;
          border-right: 1px solid #0e1a22;
        }

        @keyframes spin { to { transform: rotate(360deg); } }
        .spinner {
          width: 10px; height: 10px; border-radius: 50%;
          border: 2px solid #1a3050; border-top-color: #f5a623;
          animation: spin 0.8s linear infinite; display: inline-block;
        }
      `}</style>

      <div className="grid-bg" />
      <div className="scanline" />

      {/* ── Header ─────────────────────────────────────────────────────────── */}
      <header style={{
        position: "relative", zIndex: 10,
        borderBottom: "1px solid #0e1a22",
        background: "rgba(8,12,15,0.95)",
        backdropFilter: "blur(12px)",
      }}>
        <div style={{ maxWidth: 1100, margin: "0 auto", padding: "0 24px" }}>
          {/* Top bar */}
          <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", padding: "14px 0 0" }}>
            <div style={{ display: "flex", alignItems: "baseline", gap: 10 }}>
              <span style={{
                fontFamily: "'Syne', sans-serif", fontWeight: 800,
                fontSize: 16, letterSpacing: "0.05em", color: "#c8d8e8",
              }}>AMM CHALLENGE</span>
              <span style={{ fontSize: 9, color: "#2a4a6a", letterSpacing: "0.15em" }}>/ PROP AMM</span>
            </div>
            <div style={{ display: "flex", gap: 24, alignItems: "center" }}>
              <a href="#"
                 style={{ fontSize: 10, color: "#2a4a6a", textDecoration: "none", letterSpacing: "0.1em" }}
                 target="_blank">
                GITHUB ↗
              </a>
              <span style={{ fontSize: 10, color: "#1a3040" }}>|</span>
              <span style={{ fontSize: 10, color: "#2a4a6a", letterSpacing: "0.08em" }}>
                <a href="https://x.com/avataz_joe" target="_blank" style={{ color: "#2a4a6a", textDecoration: "none" }}>@avataz_joe</a>
              </span>
            </div>
          </div>

          {/* Nav tabs */}
          <div style={{ display: "flex", gap: 24, marginTop: 12 }}>
            {["leaderboard", "submit", "about"].map(t => (
              <button key={t} className={`nav-link${tab === t ? " active" : ""}`}
                onClick={() => { setTab(t); if (t === "submit") reset(); }}>
                {t}
              </button>
            ))}
          </div>
        </div>
      </header>

      {/* ── Main ───────────────────────────────────────────────────────────── */}
      <main style={{ position: "relative", zIndex: 5, maxWidth: 1100, margin: "0 auto", padding: "40px 24px 80px" }}>

        {/* ═══ LEADERBOARD ═══════════════════════════════════════════════════ */}
        {tab === "leaderboard" && (
          <div>
            <div style={{ marginBottom: 32 }}>
              <div className="section-label">RANKINGS</div>
              <h1 style={{
                fontFamily: "'Syne', sans-serif", fontWeight: 800,
                fontSize: 28, color: "#c8d8e8", letterSpacing: "0.02em",
              }}>Prop AMM Leaderboard</h1>
              <p style={{ marginTop: 8, fontSize: 12, color: "#3a5570", lineHeight: 1.7 }}>
                Strategies ranked by mean edge across 1,000 simulations · multi-AMM competition mode · epoch-aware capital allocation
              </p>
            </div>

            {/* Stats bar */}
            <div style={{ display: "flex", gap: 1, marginBottom: 24 }}>
              {[
                { label: "STRATEGIES", value: "47" },
                { label: "SIMULATIONS / RUN", value: "1,000" },
                { label: "STEPS / SIM", value: "10,000" },
                { label: "EPOCH LEN", value: "1,000" },
                { label: "NORMALIZER", value: "DYNAMIC" },
              ].map(s => (
                <div key={s.label} style={{
                  flex: 1, background: "#050810", border: "1px solid #0e1a22",
                  padding: "14px 16px",
                }}>
                  <div style={{ fontSize: 8, color: "#2a4060", letterSpacing: "0.2em", marginBottom: 4 }}>{s.label}</div>
                  <div style={{ fontSize: 14, color: "#c8d8e8", fontWeight: 600 }}>{s.value}</div>
                </div>
              ))}
            </div>

            {/* Table */}
            <div style={{ border: "1px solid #0e1a22", borderRadius: 4, overflow: "hidden" }}>
              <div className="lb-row header">
                <span>#</span><span>AUTHOR</span><span>STRATEGY</span>
                <span style={{ textAlign: "right" }}>AVG EDGE</span>
                <span style={{ textAlign: "right" }}>TRIES</span>
              </div>
              {LEADERBOARD_DATA.map((row, i) => (
                <div className="lb-row" key={row.rank}>
                  <span className={
                    row.rank === 1 ? "medal-1" : row.rank === 2 ? "medal-2" : row.rank === 3 ? "medal-3" : ""
                  } style={{ fontWeight: 700, fontSize: 13 }}>
                    {row.rank === 1 ? "◆" : row.rank === 2 ? "◈" : row.rank === 3 ? "◇" : row.rank}
                  </span>
                  <span>
                    <a href={`https://x.com/${row.author.replace("@","")}`} target="_blank"
                       style={{ color: "#29b6f6", textDecoration: "none", fontSize: 12 }}>
                      {row.author}
                    </a>
                    {row.model !== "None" && (
                      <span style={{ marginLeft: 8, fontSize: 9, color: "#2a4a6a",
                                     background: "#0d1a24", padding: "1px 5px", borderRadius: 2 }}>
                        AI
                      </span>
                    )}
                  </span>
                  <span style={{ color: "#8aa8c0", fontSize: 12 }}>{row.strategy}</span>
                  <span style={{ textAlign: "right", color: "#00e676", fontWeight: 600 }}>
                    +{row.edge.toFixed(2)}
                  </span>
                  <span style={{ textAlign: "right", color: "#2a4a6a" }}>{row.attempts}</span>
                </div>
              ))}
            </div>

            <div style={{ marginTop: 24, textAlign: "center" }}>
              <button className="btn-primary" onClick={() => setTab("submit")}>
                SUBMIT YOUR STRATEGY →
              </button>
            </div>
          </div>
        )}

        {/* ═══ SUBMIT ════════════════════════════════════════════════════════ */}
        {tab === "submit" && (
          <div style={{ display: "grid", gridTemplateColumns: "1fr 360px", gap: 24, alignItems: "start" }}>

            {/* Left: form */}
            <div>
              <div style={{ marginBottom: 28 }}>
                <div className="section-label">SUBMISSION</div>
                <h1 style={{
                  fontFamily: "'Syne', sans-serif", fontWeight: 800,
                  fontSize: 26, color: "#c8d8e8", letterSpacing: "0.02em",
                }}>Submit Strategy</h1>
                <p style={{ marginTop: 8, fontSize: 12, color: "#3a5570", lineHeight: 1.7 }}>
                  Upload a <span style={{ color: "#f5a623" }}>lib.rs</span> implementing <code style={{ color: "#29b6f6" }}>compute_swap</code>.
                  Server compiles, validates, and runs 1,000 simulations.
                </p>
              </div>

              {stage === null && (
                <>
                  {/* Author handle */}
                  <div style={{ marginBottom: 16 }}>
                    <div className="section-label">X / TWITTER HANDLE</div>
                    <input className="input-field" placeholder="@yourhandle"
                      value={handle} onChange={e => setHandle(e.target.value)} />
                  </div>

                  {/* File drop */}
                  <div style={{ marginBottom: 16 }}>
                    <div className="section-label" style={{ display: "flex", justifyContent: "space-between" }}>
                      <span>STRATEGY SOURCE</span>
                      {fileName && <span style={{ color: "#f5a623" }}>{fileName}</span>}
                    </div>
                    <div className={`drop-zone${dragging ? " drag" : ""}`}
                      onDragOver={e => { e.preventDefault(); setDragging(true); }}
                      onDragLeave={() => setDragging(false)}
                      onDrop={onDrop}
                      onClick={() => fileRef.current.click()}>
                      <input type="file" ref={fileRef} style={{ display: "none" }}
                        accept=".rs" onChange={e => handleFile(e.target.files[0])} />
                      <div style={{ marginBottom: 4, fontSize: 18, color: dragging ? "#f5a623" : "#1a3050" }}>⬡</div>
                      {fileName
                        ? <span style={{ color: "#f5a623" }}>{fileName} loaded · click to replace</span>
                        : <span>DROP lib.rs HERE OR CLICK TO BROWSE</span>
                      }
                    </div>
                  </div>

                  {/* Code editor */}
                  <div style={{ marginBottom: 20 }}>
                    <div className="section-label" style={{ display: "flex", justifyContent: "space-between" }}>
                      <span>CODE EDITOR</span>
                      <span style={{ color: "#1e3040" }}>{lineCount} lines</span>
                    </div>
                    <div style={{ position: "relative", background: "#050810", border: "1px solid #1a2a38", borderRadius: 4 }}>
                      {/* Line numbers */}
                      <div className="line-numbers">
                        {Array.from({ length: lineCount }, (_, i) => (
                          <div key={i}>{i + 1}</div>
                        ))}
                      </div>
                      <textarea ref={textareaRef} className="code-area"
                        value={code} onChange={e => setCode(e.target.value)}
                        rows={Math.min(Math.max(lineCount, 20), 42)}
                        spellCheck={false}
                        onKeyDown={e => {
                          if (e.key === "Tab") {
                            e.preventDefault();
                            const s = e.target.selectionStart;
                            const v = e.target.value;
                            setCode(v.substring(0, s) + "    " + v.substring(e.target.selectionEnd));
                            setTimeout(() => e.target.setSelectionRange(s + 4, s + 4), 0);
                          }
                        }}
                      />
                    </div>
                  </div>

                  {/* Restrictions */}
                  <div style={{
                    background: "#050810", border: "1px solid #0e1a22",
                    borderLeft: "2px solid #1e3a50",
                    borderRadius: "0 4px 4px 0",
                    padding: "12px 16px", marginBottom: 20,
                    fontSize: 11, color: "#2a4a6a", lineHeight: 1.8,
                  }}>
                    <div style={{ color: "#3a5570", marginBottom: 4, letterSpacing: "0.1em", fontSize: 9, textTransform: "uppercase" }}>
                      Submission constraints
                    </div>
                    <div>Single <span style={{ color: "#8aa8c0" }}>lib.rs</span> · No <code>unsafe</code> · No <code>include!()</code> / <code>env!()</code></div>
                    <div>Deps: <span style={{ color: "#8aa8c0" }}>prop_amm_submission_sdk</span> only · Max 100k CU</div>
                    <div>Monotone output · Concave in input · Exports <code>__prop_amm_compute_swap</code></div>
                  </div>

                  {error && (
                    <div style={{
                      background: "rgba(255,82,82,0.06)", border: "1px solid rgba(255,82,82,0.2)",
                      borderRadius: 4, padding: "10px 14px", marginBottom: 16,
                      fontSize: 12, color: "#ff5252",
                    }}>
                      ✗ {error}
                    </div>
                  )}

                  <div style={{ display: "flex", gap: 12, alignItems: "center" }}>
                    <button className="btn-primary" onClick={runSubmission}>
                      SUBMIT & RUN →
                    </button>
                    <button className="btn-ghost" onClick={() => setCode(STARTER_CODE)}>
                      LOAD STARTER
                    </button>
                  </div>
                </>
              )}

              {/* ── Pipeline running ───────────────────────────────────────── */}
              {stage !== null && stage !== "done" && (
                <div>
                  {/* Stage pills */}
                  <div style={{ display: "flex", gap: 4, marginBottom: 20, flexWrap: "wrap" }}>
                    {STAGES.map((s, i) => (
                      <div key={s.id} className={`stage-pill${
                        i < stageIdx ? " done" : i === stageIdx ? " active" : " pending"
                      }`}>
                        {i < stageIdx
                          ? <span>✓</span>
                          : i === stageIdx
                          ? <span className="spinner" />
                          : <span style={{ opacity: 0.3 }}>○</span>
                        }
                        <span>{s.label}</span>
                      </div>
                    ))}
                  </div>

                  {/* Log terminal */}
                  <div style={{
                    background: "#020508",
                    border: "1px solid #0e1a22",
                    borderRadius: 4,
                    padding: "16px",
                    minHeight: 280,
                    position: "relative",
                    overflow: "hidden",
                  }}>
                    <div style={{
                      position: "absolute", top: 0, left: 0, right: 0,
                      background: "#050810",
                      borderBottom: "1px solid #0e1a22",
                      padding: "6px 14px",
                      fontSize: 9, color: "#1e3040", letterSpacing: "0.15em",
                      display: "flex", justifyContent: "space-between"
                    }}>
                      <span>BUILD LOG</span>
                      <span style={{ color: "#f5a623" }}>
                        {STAGES[stageIdx]?.label} · {STAGES[stageIdx]?.desc}
                      </span>
                    </div>
                    <div ref={logRef} style={{ marginTop: 28, maxHeight: 340, overflowY: "auto" }}>
                      {stageLog.map((line, i) => (
                        <div key={i} className={`log-line ${
                          line.startsWith("✓") ? "ok" : line.startsWith("→") ? "arrow" : "info"
                        }`}>
                          {line.startsWith("✓") ? "" : line.startsWith("→") ? "" : "   "}{line}
                        </div>
                      ))}
                      <span className="ticker-char" style={{ color: "#f5a623" }}>█</span>
                    </div>
                  </div>
                </div>
              )}

              {/* ── Result ────────────────────────────────────────────────── */}
              {stage === "done" && result && (
                <div>
                  <div className="result-card">
                    <div style={{ fontSize: 10, color: "#00e676", letterSpacing: "0.2em", marginBottom: 16 }}>
                      ✓ SUBMISSION COMPLETE
                    </div>
                    <div style={{
                      fontFamily: "'Syne', sans-serif", fontWeight: 800,
                      fontSize: 48, color: "#00e676",
                      textShadow: "0 0 30px rgba(0,230,118,0.3)",
                      letterSpacing: "0.02em", marginBottom: 4,
                    }}>
                      +{result.edge.toFixed(2)}
                    </div>
                    <div style={{ fontSize: 11, color: "#3a5570", marginBottom: 24 }}>
                      MEAN EDGE · σ = {result.std.toFixed(2)}
                    </div>

                    <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr 1fr", gap: 1, marginBottom: 24 }}>
                      {[
                        { label: "VS NORMALIZER", value: `+${(result.edge - 312.44).toFixed(2)}`, color: "#00e676" },
                        { label: "SHARPE",        value: (result.edge / result.std).toFixed(3),   color: "#29b6f6" },
                        { label: "EST. RANK",     value: `#${result.rank}`,                        color: "#f5a623" },
                      ].map(m => (
                        <div key={m.label} style={{ background: "#050810", padding: "14px 0" }}>
                          <div style={{ fontSize: 8, color: "#2a4060", letterSpacing: "0.18em", marginBottom: 6 }}>{m.label}</div>
                          <div style={{ fontSize: 22, fontWeight: 700, color: m.color }}>{m.value}</div>
                        </div>
                      ))}
                    </div>

                    <div style={{ fontSize: 11, color: "#2a4a6a", marginBottom: 20 }}>
                      Submitted as <span style={{ color: "#29b6f6" }}>{handle}</span>
                    </div>

                    <div style={{ display: "flex", gap: 10, justifyContent: "center" }}>
                      <button className="btn-primary" onClick={() => setTab("leaderboard")}>
                        VIEW LEADERBOARD →
                      </button>
                      <button className="btn-ghost" onClick={reset}>
                        SUBMIT ANOTHER
                      </button>
                    </div>
                  </div>

                  {/* Full log (collapsed) */}
                  <details style={{ marginTop: 16 }}>
                    <summary style={{ fontSize: 10, color: "#2a4060", cursor: "pointer", letterSpacing: "0.1em" }}>
                      VIEW BUILD LOG ({stageLog.length} lines)
                    </summary>
                    <div style={{
                      background: "#020508", border: "1px solid #0e1a22",
                      borderRadius: 4, padding: 14, marginTop: 8,
                      maxHeight: 200, overflowY: "auto",
                    }}>
                      {stageLog.map((line, i) => (
                        <div key={i} style={{
                          fontSize: 11, lineHeight: 1.6,
                          color: line.startsWith("✓") ? "#00e676" : "#2a4a6a",
                        }}>{line}</div>
                      ))}
                    </div>
                  </details>
                </div>
              )}
            </div>

            {/* Right: sidebar ─────────────────────────────────────────────── */}
            <div style={{ display: "flex", flexDirection: "column", gap: 16 }}>

              {/* Mini leaderboard */}
              <div style={{ background: "#050810", border: "1px solid #0e1a22", borderRadius: 4 }}>
                <div style={{
                  padding: "10px 16px", borderBottom: "1px solid #0e1a22",
                  fontSize: 9, color: "#2a4060", letterSpacing: "0.18em",
                  display: "flex", justifyContent: "space-between",
                }}>
                  <span>TOP STRATEGIES</span>
                  <button onClick={() => setTab("leaderboard")}
                    style={{ background: "none", border: "none", color: "#1e3a50",
                             fontSize: 9, cursor: "pointer", letterSpacing: "0.1em", fontFamily: "inherit" }}>
                    VIEW ALL ↗
                  </button>
                </div>
                {LEADERBOARD_DATA.slice(0, 5).map(row => (
                  <div key={row.rank} style={{
                    display: "flex", justifyContent: "space-between", alignItems: "center",
                    padding: "8px 16px", borderBottom: "1px solid #0a1520",
                    fontSize: 11,
                  }}>
                    <div style={{ display: "flex", gap: 10, alignItems: "baseline" }}>
                      <span style={{ color: "#1e3040", width: 16, textAlign: "right" }}>{row.rank}</span>
                      <span style={{ color: "#607080", maxWidth: 100,
                                     overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                        {row.strategy}
                      </span>
                    </div>
                    <span style={{ color: "#00e676", fontWeight: 600 }}>+{row.edge.toFixed(1)}</span>
                  </div>
                ))}
              </div>

              {/* Requirements checklist */}
              <div style={{ background: "#050810", border: "1px solid #0e1a22", borderRadius: 4, padding: 16 }}>
                <div className="section-label" style={{ marginBottom: 12 }}>INTERFACE CHECKLIST</div>
                {[
                  { label: "const NAME: &str", ok: code.includes("NAME") },
                  { label: "const MODEL_USED: &str", ok: code.includes("MODEL_USED") },
                  { label: "fn compute_swap(ctx: &SwapContext)", ok: code.includes("compute_swap") },
                  { label: "No unsafe code", ok: !code.includes("unsafe") },
                  { label: "fn after_swap (optional)", ok: code.includes("after_swap"), optional: true },
                  { label: "fn on_epoch_boundary (optional)", ok: code.includes("on_epoch_boundary"), optional: true },
                ].map(item => (
                  <div key={item.label} style={{
                    display: "flex", gap: 8, alignItems: "baseline",
                    padding: "4px 0", borderBottom: "1px solid #0a1520",
                    fontSize: 11,
                  }}>
                    <span style={{ color: item.ok ? "#00e676" : item.optional ? "#2a4060" : "#ff5252", flexShrink: 0 }}>
                      {item.ok ? "✓" : item.optional ? "·" : "✗"}
                    </span>
                    <span style={{ color: item.ok ? "#607080" : item.optional ? "#1e3040" : "#3a2030",
                                   fontFamily: "monospace", fontSize: 10 }}>
                      {item.label}
                    </span>
                  </div>
                ))}
              </div>

              {/* New AfterSwap fields callout */}
              <div style={{
                background: "#050810",
                border: "1px solid #1a2a38",
                borderLeft: "2px solid #f5a623",
                borderRadius: "0 4px 4px 0",
                padding: 16,
              }}>
                <div style={{ fontSize: 9, color: "#f5a623", letterSpacing: "0.18em", marginBottom: 10 }}>
                  NEW IN PROP AMM MULTI
                </div>
                {[
                  { field: "flow_captured", desc: "Fraction of order routed here" },
                  { field: "capital_weight", desc: "Your share of total capital" },
                  { field: "epoch_step", desc: "Step within current epoch" },
                  { field: "competing_spot_prices[8]", desc: "Other AMMs' live spot prices" },
                  { field: "TAG_EPOCH_BOUNDARY", desc: "Hook called between epochs" },
                ].map(f => (
                  <div key={f.field} style={{ marginBottom: 8 }}>
                    <div style={{ fontSize: 10, color: "#29b6f6", marginBottom: 1 }}>{f.field}</div>
                    <div style={{ fontSize: 10, color: "#2a4060", lineHeight: 1.5 }}>{f.desc}</div>
                  </div>
                ))}
              </div>

              {/* Quick links */}
              <div style={{ background: "#050810", border: "1px solid #0e1a22", borderRadius: 4, padding: 16 }}>
                <div className="section-label" style={{ marginBottom: 10 }}>RESOURCES</div>
                {[
                  { label: "Engine source (GitHub)", url: "#" },
                  { label: "Strategy SDK docs", url: "#" },
                  { label: "Starter lib.rs", url: "#" },
                  { label: "Routing math explained", url: "#" },
                ].map(l => (
                  <a key={l.label} href={l.url} target="_blank"
                     style={{ display: "block", color: "#2a4a6a", textDecoration: "none",
                              fontSize: 11, padding: "5px 0", borderBottom: "1px solid #0a1520",
                              transition: "color 0.15s" }}
                     onMouseOver={e => e.target.style.color = "#29b6f6"}
                     onMouseOut={e => e.target.style.color = "#2a4a6a"}>
                    → {l.label}
                  </a>
                ))}
              </div>
            </div>
          </div>
        )}

        {/* ═══ ABOUT ═════════════════════════════════════════════════════════ */}
        {tab === "about" && (
          <div style={{ maxWidth: 700 }}>
            <div className="section-label">DOCUMENTATION</div>
            <h1 style={{
              fontFamily: "'Syne', sans-serif", fontWeight: 800,
              fontSize: 26, color: "#c8d8e8", marginBottom: 24,
            }}>Prop AMM Multi · How It Works</h1>

            {[
              {
                title: "What you're building",
                body: `Write a pricing function in Rust. Your AMM competes against N other submitted strategies and a dynamic normalizer in a shared retail flow market. The router splits each retail order optimally across all AMMs — whoever offers the best marginal price wins the flow.`,
              },
              {
                title: "N-way equimarginal routing",
                body: `Flow is not sent entirely to the cheapest pool. The router finds the shadow price λ* such that the sum of optimal allocations across all pools equals the total order size. This is the classic equimarginal principle — marginal output rates are equalized at the optimum. Strategies that offer a flat price curve (CPAMM) will share flow; strategies that offer sharper quotes around fair value attract more flow at the margin.`,
              },
              {
                title: "Epoch-based capital allocation",
                body: `Every 1,000 steps, your strategy's reserves are rescaled based on risk-adjusted epoch edge: score = edge − λ·max(0,−edge). This asymmetric scoring penalizes losing epochs 3× harder than it rewards winning ones. Winners get more capital in the next epoch, which compounds through routing (more reserves → sharper prices → more flow).`,
              },
              {
                title: "New AfterSwap signals",
                body: `After every trade, your strategy receives: flow_captured (what fraction of this order was routed to you), competing_spot_prices (every other AMM's current spot), capital_weight (your share of total protocol capital), and epoch_step / epoch_number. Use these to detect whether you're winning or losing routing competition and adapt.`,
              },
              {
                title: "Epoch boundary hook",
                body: `At each epoch transition, your strategy receives TAG_EPOCH_BOUNDARY with your epoch edge, cumulative edge, new reserves, and new capital weight. Use this to reinitialize your vol estimate, adjust aggressiveness, or log internal state — storage persists untouched across the boundary.`,
              },
            ].map(section => (
              <div key={section.title} style={{ marginBottom: 32 }}>
                <div style={{ fontSize: 12, color: "#f5a623", letterSpacing: "0.06em", marginBottom: 8, fontWeight: 600 }}>
                  {section.title}
                </div>
                <div style={{ fontSize: 13, color: "#607080", lineHeight: 1.8 }}>{section.body}</div>
              </div>
            ))}

            <button className="btn-primary" onClick={() => setTab("submit")}>
              SUBMIT YOUR STRATEGY →
            </button>
          </div>
        )}
      </main>

      {/* ── Footer ─────────────────────────────────────────────────────────── */}
      <footer style={{
        position: "relative", zIndex: 5,
        borderTop: "1px solid #0e1a22",
        padding: "20px 24px",
        display: "flex", justifyContent: "center", gap: 40,
        fontSize: 9, color: "#1a2a38", letterSpacing: "0.15em",
      }}>
        <span>AMM CHALLENGE</span>
        <span>PROP AMM MULTI EDITION</span>
        <span>
          <a href="https://x.com/avataz_joe" target="_blank" style={{ color: "#1a2a38", textDecoration: "none" }}>@AVATAZ_JOE</a>
        </span>
      </footer>
    </div>
  );
}
