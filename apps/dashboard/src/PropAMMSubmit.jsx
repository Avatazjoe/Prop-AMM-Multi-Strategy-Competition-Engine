import React, { useState, useRef, useEffect, useCallback } from "react";

const STARTER_CODE = `const NAME: &str = "dashboard_starter_30bps";
const FEE_BPS: u128 = 30;

#[no_mangle]
pub extern "C" fn __prop_amm_compute_swap(data: *const u8, len: usize) -> u64 {
  let bytes = unsafe { std::slice::from_raw_parts(data, len) };
  if bytes.len() < 25 { return 0; }

  let input = u64::from_le_bytes(bytes[1..9].try_into().unwrap_or([0; 8]));
  let rx = u64::from_le_bytes(bytes[9..17].try_into().unwrap_or([0; 8]));
  let ry = u64::from_le_bytes(bytes[17..25].try_into().unwrap_or([0; 8]));
  let is_buy = bytes[0] == 0;

  if is_buy { cpamm_output(input, ry, rx, FEE_BPS) } else { cpamm_output(input, rx, ry, FEE_BPS) }
}

#[no_mangle]
pub extern "C" fn __prop_amm_after_swap(_data: *const u8, _len: usize, _storage_ptr: *mut u8) {}

#[no_mangle]
pub extern "C" fn __prop_amm_get_name(buf: *mut u8, max_len: usize) -> usize {
  let bytes = NAME.as_bytes();
  let n = bytes.len().min(max_len);
  unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf, n) };
  n
}

fn cpamm_output(input: u64, reserve_in: u64, reserve_out: u64, fee_bps: u128) -> u64 {
  if input == 0 || reserve_in == 0 || reserve_out == 0 { return 0; }
  let fee_den = 10_000u128;
  let input_eff = (input as u128) * (fee_den - fee_bps) / fee_den;
  let denom = reserve_in as u128 + input_eff;
  if denom == 0 { return 0; }
  ((reserve_out as u128) * input_eff / denom) as u64
}`;

// ─── Pipeline stages ──────────────────────────────────────────────────────────
const STAGES = [
  { id: "upload",   label: "UPLOAD",   desc: "Source received" },
  { id: "compile",  label: "COMPILE",  desc: "Rust → native + BPF" },
  { id: "validate", label: "VALIDATE", desc: "Monotone · Concave · Symbols" },
  { id: "simulate", label: "SIMULATE", desc: "1,000 × 10,000 steps" },
  { id: "score",    label: "SCORE",    desc: "Edge vs normalizer" },
];

const toFiniteNumber = (value, fallback = 0) => {
  const num = Number(value);
  return Number.isFinite(num) ? num : fallback;
};

const stripRustComments = (source) => source
  .replace(/\/\*[\s\S]*?\*\//g, "")
  .replace(/\/\/.*$/gm, "");

// ─── Component ────────────────────────────────────────────────────────────────
export default function PropAMMSubmit() {
  const [tab, setTab] = useState("submit"); // submit | leaderboard
  const [code, setCode] = useState("");
  const [apiBase, setApiBase] = useState(import.meta.env.VITE_API_BASE_URL || "http://127.0.0.1:18002");
  const [handle, setHandle] = useState("@avataz_joe");
  const [fileName, setFileName] = useState(null);
  const [simulations, setSimulations] = useState(1000);
  const [steps, setSteps] = useState(2000);
  const [epochLen, setEpochLen] = useState(500);
  const [seedStart, setSeedStart] = useState(0);
  const [dragging, setDragging] = useState(false);
  const [stage, setStage] = useState(null); // null | stage id | "done" | "error"
  const [stageIdx, setStageIdx] = useState(-1);
  const [stageLog, setStageLog] = useState([]);
  const [result, setResult] = useState(null);
  const [error, setError] = useState(null);
  const [jobId, setJobId] = useState(null);
  const [jobStatus, setJobStatus] = useState(null);
  const [leaderboard, setLeaderboard] = useState([]);
  const [stats, setStats] = useState({
    strategies: 47,
    simulations: 1000,
    steps: 10000,
    epoch_len: 1000,
    normalizer: "DYNAMIC",
  });
  const [lineCount, setLineCount] = useState(0);
  const fileRef = useRef();
  const logRef = useRef();
  const textareaRef = useRef();

  const codeForChecks = stripRustComments(code);
  const interfaceChecklist = [
    {
      label: "const NAME: &str",
      ok: /\bconst\s+NAME\s*:\s*&str\b/.test(codeForChecks),
    },
    {
      label: "fn __prop_amm_compute_swap(..)",
      ok: /\bfn\s+__prop_amm_compute_swap\s*\(/.test(codeForChecks),
    },
    {
      label: "fn __prop_amm_get_name(..)",
      ok: /\bfn\s+__prop_amm_get_name\s*\(/.test(codeForChecks),
    },
    {
      label: "No include!()/env!()",
      ok: !/\binclude!\s*\(|\benv!\s*\(/.test(codeForChecks),
    },
    {
      label: "fn __prop_amm_after_swap(..) (optional)",
      ok: /\bfn\s+__prop_amm_after_swap\s*\(/.test(codeForChecks),
      optional: true,
    },
    {
      label: "fn on_epoch_boundary(..) (optional)",
      ok: /\bfn\s+on_epoch_boundary\s*\(/.test(codeForChecks),
      optional: true,
    },
  ];
  const requiredChecklist = interfaceChecklist.filter((item) => !item.optional);
  const passedRequiredCount = requiredChecklist.filter((item) => item.ok).length;
  const allRequiredChecksPassed = passedRequiredCount === requiredChecklist.length;

  const leaderboardRows = leaderboard.slice(0, 10).map((row, idx) => ({
    rank: idx + 1,
    author: row.author ? String(row.author) : null,
    strategy: row.strategy_name ? String(row.strategy_name) : `submission_${idx + 1}`,
    edge: toFiniteNumber(row.mean_edge, 0),
    attempts: Math.max(1, Math.floor(toFiniteNumber(row.attempts, 1))),
    model: "None",
  }));

  useEffect(() => {
    setLineCount(code.split("\n").length);
  }, [code]);

  useEffect(() => {
    if (logRef.current) {
      logRef.current.scrollTop = logRef.current.scrollHeight;
    }
  }, [stageLog]);

  useEffect(() => {
    if (!jobId) return;
    const timer = setInterval(async () => {
      try {
        const [statusRes, logsRes] = await Promise.all([
          fetch(`${apiBase}/api/jobs/${jobId}`),
          fetch(`${apiBase}/api/jobs/${jobId}/logs`),
        ]);
        if (!statusRes.ok || !logsRes.ok) return;
        const statusData = await statusRes.json();
        const logsText = await logsRes.text();

        const lines = logsText.split("\n").filter(Boolean);
        setStageLog(lines);
        setJobStatus(statusData.status);

        let idx = 0;
        if (logsText.includes("Compiling") || logsText.includes("Finished `dev` profile")) idx = 1;
        if (logsText.includes("[PASS]")) idx = 2;
        if (logsText.includes("Running `target/debug/prop-amm-multi")) idx = 3;
        if (logsText.includes("Strategy") && logsText.includes("Mean Edge")) idx = 4;

        setStageIdx(idx);
        setStage(STAGES[idx]?.id || "upload");

        if (statusData.status === "completed") {
          const rowMatch = logsText.match(/\n([A-Za-z0-9_\-.]+)\s+(-?\d+\.\d+)\s+(\d+\.\d+)\s+(-?\d+\.\d+)\s+(-?\d+\.\d+)/);
          const edge = rowMatch ? Number(rowMatch[2]) : 0;
          const std = rowMatch ? Number(rowMatch[3]) : 1;
          const rank = rowMatch ? 1 : 0;
          setResult({ edge, std, rank, handle });
          setStage("done");
        } else if (statusData.status === "failed") {
          setStage("error");
          setError(statusData.error_message || "Submission job failed.");
        }
      } catch {
      }
    }, 1500);

    return () => clearInterval(timer);
  }, [apiBase, handle, jobId]);

  useEffect(() => {
    const timer = setInterval(async () => {
      try {
        const [rowsRes, statsRes] = await Promise.all([
          fetch(`${apiBase}/api/leaderboard`),
          fetch(`${apiBase}/api/stats`),
        ]);
        if (!rowsRes.ok) return;
        const rows = await rowsRes.json();
        setLeaderboard(rows);
        if (statsRes.ok) {
          const nextStats = await statsRes.json();
          setStats(nextStats);
        }
      } catch {
      }
    }, 4000);
    return () => clearInterval(timer);
  }, [apiBase]);

  const statsCards = [
    { label: "STRATEGIES", value: String(stats.strategies ?? leaderboardRows.length) },
    { label: "SIMULATIONS / RUN", value: Number(stats.simulations ?? 1000).toLocaleString() },
    { label: "STEPS / SIM", value: Number(stats.steps ?? 10000).toLocaleString() },
    { label: "EPOCH LEN", value: Number(stats.epoch_len ?? 1000).toLocaleString() },
    { label: "NORMALIZER", value: String(stats.normalizer || "DYNAMIC") },
  ];

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

    const missingRequired = interfaceChecklist.filter((item) => !item.optional && !item.ok).map((item) => item.label);
    if (missingRequired.length > 0) {
      setError(`Missing required interface: ${missingRequired[0]}`);
      return;
    }

    setError(null);
    setResult(null);
    setStageLog([]);
    setStage("upload");
    setStageIdx(0);
    pushLog(`→ Received ${fileName || "strategy.rs"} (${(new Blob([code]).size / 1024).toFixed(1)} KB)`);
    pushLog("→ Author: " + handle);
    pushLog("→ Queued for backend execution");

    try {
      const response = await fetch(`${apiBase}/api/jobs`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          job_type: "submit",
          strategy_files: [],
          submitter_handle: handle.trim(),
          source_code: code,
          source_filename: fileName || `${handle.replace(/[^a-zA-Z0-9_]/g, "_") || "strategy"}.rs`,
          simulations,
          steps,
          epoch_len: epochLen,
          seed_start: seedStart,
        }),
      });

      if (!response.ok) {
        const text = await response.text();
        throw new Error(text || "Failed to create submission job");
      }

      const payload = await response.json();
      setJobId(payload.job_id);
      setJobStatus(payload.status);
    } catch (err) {
      setStage("error");
      setError(String(err));
    }
  };

  const reset = () => {
    setStage(null); setStageIdx(-1); setStageLog([]);
    setResult(null); setError(null); setFileName(null);
    setJobId(null); setJobStatus(null);
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
              {statsCards.map(s => (
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
              {leaderboardRows.map((row, i) => (
                <div className="lb-row" key={row.rank}>
                  <span className={
                    row.rank === 1 ? "medal-1" : row.rank === 2 ? "medal-2" : row.rank === 3 ? "medal-3" : ""
                  } style={{ fontWeight: 700, fontSize: 13 }}>
                    {row.rank === 1 ? "◆" : row.rank === 2 ? "◈" : row.rank === 3 ? "◇" : row.rank}
                  </span>
                  <span>
                    {row.author ? (
                      <a href={`https://x.com/${row.author.replace("@", "")}`} target="_blank"
                         style={{ color: "#29b6f6", textDecoration: "none", fontSize: 12 }}>
                        {row.author}
                      </a>
                    ) : (
                      <span style={{ color: "#607080", fontSize: 12 }}>—</span>
                    )}
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

                  <div style={{ marginBottom: 16 }}>
                    <div className="section-label">API BASE URL</div>
                    <input className="input-field" placeholder="http://localhost:8000"
                      value={apiBase} onChange={e => setApiBase(e.target.value)} />
                  </div>

                  <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 10, marginBottom: 16 }}>
                    <div>
                      <div className="section-label">SIMULATIONS</div>
                      <input className="input-field" type="number" min="1"
                        value={simulations} onChange={e => setSimulations(Number(e.target.value) || 1)} />
                    </div>
                    <div>
                      <div className="section-label">STEPS</div>
                      <input className="input-field" type="number" min="1"
                        value={steps} onChange={e => setSteps(Number(e.target.value) || 1)} />
                    </div>
                    <div>
                      <div className="section-label">EPOCH LEN</div>
                      <input className="input-field" type="number" min="1"
                        value={epochLen} onChange={e => setEpochLen(Number(e.target.value) || 1)} />
                    </div>
                    <div>
                      <div className="section-label">SEED START</div>
                      <input className="input-field" type="number" min="0"
                        value={seedStart} onChange={e => setSeedStart(Number(e.target.value) || 0)} />
                    </div>
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
                    <button className="btn-primary" onClick={runSubmission} disabled={!allRequiredChecksPassed}>
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
                        {jobId ? `JOB ${jobId} · ${jobStatus || "queued"} · ` : ""}{STAGES[stageIdx]?.label} · {STAGES[stageIdx]?.desc}
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
                {leaderboardRows.slice(0, 5).map(row => (
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
                <div style={{ display: "flex", justifyContent: "space-between", alignItems: "baseline", marginBottom: 12 }}>
                  <div className="section-label" style={{ marginBottom: 0 }}>INTERFACE CHECKLIST</div>
                  <span style={{
                    fontSize: 10,
                    color: allRequiredChecksPassed ? "#00e676" : "#f5a623",
                    letterSpacing: "0.08em",
                  }}>
                    REQUIRED {passedRequiredCount}/{requiredChecklist.length}
                  </span>
                </div>
                {interfaceChecklist.map(item => (
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
