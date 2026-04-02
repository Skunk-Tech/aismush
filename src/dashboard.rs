//! Rich HTML dashboard with live-updating stats, cost tracking,
//! request history, memory viewer, and compression metrics.

use crate::db::Db;

/// Render the main dashboard page.
pub async fn render(db: &Option<Db>, port: u16) -> String {
    let stats_json = if let Some(ref database) = db {
        crate::db::get_stats(database).await.to_string()
    } else {
        r#"{"total_requests":0}"#.to_string()
    };

    format!(r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<title>AISmush Dashboard</title>
<style>
:root {{
  --bg: #0d1117; --card: #161b22; --border: #30363d;
  --text: #c9d1d9; --dim: #8b949e; --blue: #58a6ff;
  --green: #3fb950; --purple: #bc8cff; --yellow: #d29922;
  --red: #f85149; --font: 'SF Mono','Fira Code','Consolas',monospace;
}}
* {{ margin:0; padding:0; box-sizing:border-box; }}
body {{ font-family:var(--font); background:var(--bg); color:var(--text); padding:24px; }}
h1 {{ color:var(--blue); font-size:20px; margin-bottom:4px; }}
.sub {{ color:var(--dim); font-size:12px; margin-bottom:20px; }}
.tabs {{ display:flex; gap:8px; margin-bottom:20px; }}
.tab {{ padding:6px 16px; border-radius:6px; cursor:pointer; font-size:13px;
        background:var(--card); border:1px solid var(--border); color:var(--dim); }}
.tab.active {{ background:var(--blue); color:#fff; border-color:var(--blue); }}
.grid {{ display:grid; grid-template-columns:repeat(auto-fit,minmax(160px,1fr)); gap:10px; margin-bottom:20px; }}
.card {{ background:var(--card); border:1px solid var(--border); border-radius:8px; padding:14px; }}
.card .l {{ color:var(--dim); font-size:10px; text-transform:uppercase; letter-spacing:1px; }}
.card .v {{ font-size:22px; font-weight:bold; margin-top:2px; }}
.green {{ color:var(--green); }} .blue {{ color:var(--blue); }} .purple {{ color:var(--purple); }}
.yellow {{ color:var(--yellow); }} .red {{ color:var(--red); }}
.bar-bg {{ background:#21262d; border-radius:4px; height:28px; position:relative; overflow:hidden; margin:8px 0; }}
.bar-fill {{ background:linear-gradient(90deg,var(--green),#2ea043); height:100%; border-radius:4px;
             display:flex; align-items:center; justify-content:center; font-weight:bold; font-size:12px; color:#fff;
             transition:width 0.5s; min-width:40px; }}
table {{ width:100%; border-collapse:collapse; font-size:12px; }}
th {{ text-align:left; color:var(--dim); padding:8px; border-bottom:1px solid var(--border);
      font-size:10px; text-transform:uppercase; letter-spacing:1px; }}
td {{ padding:8px; border-bottom:1px solid #21262d; }}
.tag {{ display:inline-block; padding:2px 8px; border-radius:12px; font-size:10px; font-weight:600; }}
.tag.claude {{ background:#1f1d2e; color:var(--purple); }}
.tag.deepseek {{ background:#0d2818; color:var(--green); }}
.section {{ background:var(--card); border:1px solid var(--border); border-radius:8px; padding:16px; margin-bottom:16px; }}
.section h2 {{ font-size:14px; color:var(--blue); margin-bottom:12px; }}
.mem-item {{ padding:8px; border-bottom:1px solid #21262d; display:flex; justify-content:space-between; align-items:center; }}
.mem-item:last-child {{ border:none; }}
.mem-cat {{ font-size:10px; padding:2px 6px; border-radius:4px; background:#21262d; color:var(--dim); }}
.mem-text {{ flex:1; margin:0 12px; font-size:12px; }}
.btn {{ padding:4px 10px; border-radius:4px; border:1px solid var(--border); background:var(--card);
        color:var(--dim); cursor:pointer; font-size:11px; font-family:var(--font); }}
.btn:hover {{ border-color:var(--blue); color:var(--blue); }}
.btn.danger:hover {{ border-color:var(--red); color:var(--red); }}
#page-history, #page-search, #page-memories {{ display:none; }}
.footer {{ margin-top:24px; color:var(--dim); font-size:11px; text-align:center; }}
</style>
</head>
<body>

<h1>AISmush</h1>
<p class="sub">Smart routing · Context compression · Persistent memory · Cost tracking</p>

<div class="tabs">
  <div class="tab active" onclick="showPage('overview',this)">Overview</div>
  <div class="tab" onclick="showPage('history',this)">History</div>
  <div class="tab" onclick="showPage('search',this)">Search</div>
  <div class="tab" onclick="showPage('memories',this)">Memories</div>
</div>

<!-- Overview Page -->
<div id="page-overview">
  <div class="grid" id="stats-grid"></div>

  <div style="display:grid;grid-template-columns:1fr 1fr;gap:12px;margin-bottom:16px">
    <div class="section" style="margin:0">
      <h2 style="font-size:13px">Compression Savings</h2>
      <p style="font-size:11px;color:var(--dim);margin-bottom:8px">Tokens saved by stripping comments, dedup, truncation. Works in ALL modes.</p>
      <div style="font-size:20px;font-weight:bold;color:var(--green)" id="comp-tokens-saved">0</div>
      <div style="font-size:11px;color:var(--dim)">tokens saved</div>
      <div style="font-size:14px;font-weight:bold;color:var(--yellow);margin-top:4px" id="comp-cost-saved">$0</div>
      <div style="font-size:11px;color:var(--dim)">estimated cost saved</div>
    </div>
    <div class="section" style="margin:0">
      <h2 style="font-size:13px">Routing Savings</h2>
      <p style="font-size:11px;color:var(--dim);margin-bottom:8px">Money saved by sending mechanical turns to DeepSeek instead of Claude.</p>
      <div style="font-size:20px;font-weight:bold;color:var(--green)" id="routing-saved">$0</div>
      <div style="font-size:11px;color:var(--dim)">saved vs all-Claude</div>
      <div style="font-size:14px;font-weight:bold;margin-top:4px" id="routing-pct">0%</div>
      <div style="font-size:11px;color:var(--dim)" id="routing-hint"></div>
    </div>
  </div>

  <div class="section">
    <h2 style="font-size:13px">Total</h2>
    <div class="bar-bg"><div class="bar-fill" id="savings-bar" style="width:0%">0%</div></div>
    <div style="display:flex;justify-content:space-between;font-size:12px;color:var(--dim)">
      <span>Actual: <span class="yellow" id="actual-cost">$0</span></span>
      <span>All-Claude: <span class="red" id="equiv-cost">$0</span></span>
      <span>Total Saved: <span class="green" id="saved-cost">$0</span></span>
    </div>
  </div>

  <div class="section">
    <h2>Recent Requests</h2>
    <table>
      <thead><tr><th>Time</th><th>Provider</th><th>Model</th><th>Reason</th><th>Tokens</th><th>Cost</th><th>Latency</th></tr></thead>
      <tbody id="recent-table"></tbody>
    </table>
  </div>
</div>

<!-- History Page -->
<!-- Search Page -->
<div id="page-search">
  <div class="section">
    <h2>Search Past Conversations</h2>
    <div style="display:flex;gap:8px;margin-bottom:16px">
      <input type="text" id="search-query" placeholder="Search by meaning... e.g. 'auth bug fix'" style="flex:1;padding:8px 12px;background:var(--bg);border:1px solid var(--border);border-radius:6px;color:var(--text);font-family:var(--font);font-size:13px" onkeydown="if(event.key==='Enter')runSearch()">
      <button class="btn" onclick="runSearch()" style="padding:8px 16px">Search</button>
    </div>
    <div id="search-results"><p style="color:var(--dim);font-size:12px">Enter a query to search your conversation history.</p></div>
  </div>
</div>

<div id="page-history">
  <div class="section">
    <h2>Request History</h2>
    <table>
      <thead><tr><th>Time</th><th>Provider</th><th>Model</th><th>Reason</th><th>In/Out Tokens</th><th>Cost</th><th>Saved</th><th>Latency</th></tr></thead>
      <tbody id="history-table"></tbody>
    </table>
  </div>
</div>

<!-- Memories Page -->
<div id="page-memories">
  <div class="section">
    <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:12px">
      <h2 style="margin:0">Project Memories</h2>
      <button class="btn danger" onclick="clearMemories()">Clear All</button>
    </div>
    <div id="memories-list"><p style="color:var(--dim);font-size:12px">Loading...</p></div>
  </div>
</div>

<div class="footer">
  <a href="/stats" style="color:var(--blue)">JSON Stats</a> ·
  <a href="/history" style="color:var(--blue)">JSON History</a> ·
  <a href="/memories" style="color:var(--blue)">JSON Memories</a> ·
  Port {port}
</div>

<script>
const PORT = {port};
let currentPage = 'overview';

function showPage(page, el) {{
  document.querySelectorAll('[id^=page-]').forEach(e => e.style.display = 'none');
  document.querySelectorAll('.tab').forEach(e => e.classList.remove('active'));
  document.getElementById('page-' + page).style.display = 'block';
  if (el) el.classList.add('active');
  currentPage = page;
  if (page === 'history') loadHistory();
  if (page === 'memories') loadMemories();
  if (page === 'search') document.getElementById('search-query').focus();
}}

async function runSearch() {{
  const query = document.getElementById('search-query').value.trim();
  if (!query) return;
  const results = document.getElementById('search-results');
  results.innerHTML = '<p style="color:var(--dim)">Searching...</p>';
  try {{
    const r = await fetch('/search?q=' + encodeURIComponent(query));
    const data = await r.json();
    if (!data || data.length === 0) {{
      results.innerHTML = '<p style="color:var(--dim)">No results found.</p>';
      return;
    }}
    results.innerHTML = data.map(r => `
      <div style="padding:12px;border-bottom:1px solid #21262d">
        <div style="display:flex;justify-content:space-between;margin-bottom:4px">
          <span style="color:var(--dim);font-size:11px">${{fmtTime(r.timestamp)}} · ${{r.project_path}} · <span class="tag ${{r.provider}}">${{r.provider}}</span></span>
          <span style="color:var(--dim);font-size:11px">score: ${{r.similarity_score.toFixed(2)}}</span>
        </div>
        <div style="font-size:13px;margin-bottom:4px"><strong>You:</strong> ${{r.user_message}}</div>
        <div style="font-size:12px;color:var(--dim)"><strong>AI:</strong> ${{r.assistant_snippet}}</div>
        ${{r.tools_used.length > 0 ? `<div style="font-size:11px;color:var(--dim);margin-top:4px">Tools: ${{r.tools_used.join(', ')}}</div>` : ''}}
      </div>
    `).join('');
  }} catch(e) {{
    results.innerHTML = '<p style="color:var(--red)">Search failed: ' + e.message + '</p>';
  }}
}}

function fmt(n) {{ return '$' + n.toFixed(4); }}
function fmtK(n) {{ return n > 1e6 ? (n/1e6).toFixed(1)+'M' : n > 1e3 ? (n/1e3).toFixed(1)+'K' : n.toString(); }}
function fmtTime(ts) {{ return new Date(ts * 1000).toLocaleTimeString(); }}

async function refresh() {{
  try {{
    const r = await fetch('/stats');
    const s = await r.json();

    const grid = document.getElementById('stats-grid');
    grid.innerHTML = `
      <div class="card"><div class="l">Requests</div><div class="v blue">${{s.total_requests||0}}</div></div>
      <div class="card"><div class="l">Claude</div><div class="v purple">${{s.claude_turns||0}}</div></div>
      <div class="card"><div class="l">DeepSeek</div><div class="v green">${{s.deepseek_turns||0}}</div></div>
      <div class="card"><div class="l">Input Tokens</div><div class="v">${{fmtK(s.total_input_tokens||0)}}</div></div>
      <div class="card"><div class="l">Output Tokens</div><div class="v">${{fmtK(s.total_output_tokens||0)}}</div></div>
      <div class="card"><div class="l">Avg Latency</div><div class="v">${{s.avg_latency_ms||0}}ms</div></div>
      <div class="card"><div class="l">Compressed</div><div class="v yellow">${{s.compressed_requests||0}}</div></div>
      <div class="card"><div class="l">Context Saved</div><div class="v green">${{fmtK((s.compressed_original_bytes||0)-(s.compressed_final_bytes||0))}} B</div></div>
    `;

    const pct = s.savings_percent || 0;
    document.getElementById('savings-bar').style.width = Math.min(pct, 100) + '%';
    document.getElementById('savings-bar').textContent = pct.toFixed(1) + '% saved';
    document.getElementById('actual-cost').textContent = fmt(s.actual_cost || 0);
    document.getElementById('equiv-cost').textContent = fmt(s.claude_equiv_cost || 0);
    document.getElementById('saved-cost').textContent = fmt(s.savings || 0);

    // Compression savings
    const compOrig = s.compressed_original_bytes || 0;
    const compFinal = s.compressed_final_bytes || 0;
    const compTokensSaved = Math.round((compOrig - compFinal) / 4); // ~4 chars per token
    const compCostSaved = compTokensSaved * 3.0 / 1000000; // Sonnet input pricing
    document.getElementById('comp-tokens-saved').textContent = fmtK(compTokensSaved);
    document.getElementById('comp-cost-saved').textContent = fmt(compCostSaved);

    // Routing savings
    const routingSaved = s.savings || 0;
    const routingPct = s.savings_percent || 0;
    const potential = s.potential_routing_savings || 0;
    document.getElementById('routing-saved').textContent = fmt(routingSaved);
    const routingPctEl = document.getElementById('routing-pct');
    const routingHint = document.getElementById('routing-hint');
    if (s.deepseek_turns > 0) {{
      routingPctEl.textContent = routingPct.toFixed(1) + '% cheaper';
      routingPctEl.style.color = 'var(--green)';
      routingHint.textContent = 'Smart routing active';
    }} else if (potential > 0) {{
      routingPctEl.textContent = fmt(potential) + ' potential';
      routingPctEl.style.color = 'var(--yellow)';
      routingHint.textContent = 'Enable smart routing to unlock this';
    }} else {{
      routingPctEl.textContent = 'N/A';
      routingPctEl.style.color = 'var(--dim)';
      routingHint.textContent = '';
    }}

    // Load recent requests
    const hr = await fetch('/history');
    const hist = await hr.json();
    const tbody = document.getElementById('recent-table');
    tbody.innerHTML = hist.slice(0, 10).map(r => `
      <tr>
        <td>${{fmtTime(r.timestamp)}}</td>
        <td><span class="tag ${{r.provider}}">${{r.provider}}</span></td>
        <td>${{r.model}}</td>
        <td>${{r.reason}}</td>
        <td>${{fmtK(r.input_tokens)}}/${{fmtK(r.output_tokens)}}</td>
        <td>${{fmt(r.actual_cost)}}</td>
        <td>${{r.latency_ms}}ms</td>
      </tr>
    `).join('');
  }} catch(e) {{ console.error('Refresh failed:', e); }}
}}

async function loadHistory() {{
  try {{
    const r = await fetch('/history');
    const hist = await r.json();
    if (hist.length === 0) {{
      document.getElementById('history-table').innerHTML = '<tr><td colspan="8" style="color:var(--dim)">No requests recorded yet</td></tr>';
      return;
    }}
    document.getElementById('history-table').innerHTML = hist.map(r => `
      <tr>
        <td>${{fmtTime(r.timestamp)}}</td>
        <td><span class="tag ${{r.provider}}">${{r.provider}}</span></td>
        <td>${{r.model}}</td>
        <td>${{r.reason}}</td>
        <td>${{fmtK(r.input_tokens)}}/${{fmtK(r.output_tokens)}}</td>
        <td>${{fmt(r.actual_cost)}}</td>
        <td>${{fmt((r.equiv_cost||0) - (r.actual_cost||0))}}</td>
        <td>${{r.latency_ms}}ms</td>
      </tr>
    `).join('');
  }} catch(e) {{ console.error('History load failed:', e); }}
}}

async function loadMemories() {{
  try {{
    const r = await fetch('/memories');
    const mems = await r.json();
    const list = document.getElementById('memories-list');
    if (!mems || mems.length === 0) {{
      list.innerHTML = '<p style="color:var(--dim);font-size:12px">No memories yet. Memories are extracted from AI responses during coding sessions.</p>';
      return;
    }}
    list.innerHTML = mems.map(m => `
      <div class="mem-item">
        <span class="mem-cat">${{m.category}}</span>
        <span class="mem-text">${{m.content}}</span>
        <span style="color:var(--dim);font-size:10px">${{m.accesses||0}} hits · score ${{(m.relevance||0).toFixed(2)}}</span>
      </div>
    `).join('');
  }} catch(e) {{ console.error('Memories load failed:', e); }}
}}

async function clearMemories() {{
  if (!confirm('Clear all memories?')) return;
  await fetch('/memories/clear', {{ method: 'POST' }});
  loadMemories();
}}

// Auto-refresh every 5s
refresh();
setInterval(() => {{ if (currentPage === 'overview') refresh(); }}, 5000);
</script>
</body>
</html>"##, port = port)
}
