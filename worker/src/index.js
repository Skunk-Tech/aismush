/**
 * AISmush Global Stats Worker
 *
 * Receives anonymous stats from AISmush proxies worldwide,
 * aggregates them, and serves the totals.
 *
 * No personal data, no API keys, no prompts — just numbers.
 */

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    const cors = {
      'Access-Control-Allow-Origin': '*',
      'Access-Control-Allow-Methods': 'GET, POST, OPTIONS',
      'Access-Control-Allow-Headers': 'Content-Type',
    };

    // CORS preflight
    if (request.method === 'OPTIONS') {
      return new Response(null, { headers: cors });
    }

    // GET /stats — return aggregated global stats
    if ((url.pathname === '/api/stats' || url.pathname === '/stats') && request.method === 'GET') {
      const stats = await getGlobalStats(env.STATS);
      return new Response(JSON.stringify(stats), {
        headers: { 'Content-Type': 'application/json', ...cors },
      });
    }

    // POST /report — receive stats from an AISmush instance
    if ((url.pathname === '/api/report' || url.pathname === '/report') && request.method === 'POST') {
      try {
        const body = await request.json();

        // Validate — only accept the fields we expect
        const report = {
          requests: Math.max(0, parseInt(body.requests) || 0),
          claude_turns: Math.max(0, parseInt(body.claude_turns) || 0),
          deepseek_turns: Math.max(0, parseInt(body.deepseek_turns) || 0),
          routing_savings: Math.max(0, parseFloat(body.routing_savings || body.savings) || 0),
          compression_savings: Math.max(0, parseFloat(body.compression_savings) || 0),
          compressed_tokens: Math.max(0, parseInt(body.compressed_tokens || body.compressed_bytes) || 0),
          version: String(body.version || 'unknown').slice(0, 20),
          instance_id: String(body.instance_id || '').slice(0, 40),
        };

        // Reject obviously fake data
        if (report.requests > 100000 || report.routing_savings > 10000) {
          return new Response(JSON.stringify({ error: 'invalid data' }), {
            status: 400, headers: { 'Content-Type': 'application/json', ...cors },
          });
        }

        await recordReport(env.STATS, report);

        return new Response(JSON.stringify({ ok: true }), {
          headers: { 'Content-Type': 'application/json', ...cors },
        });
      } catch (e) {
        return new Response(JSON.stringify({ error: 'bad request' }), {
          status: 400, headers: { 'Content-Type': 'application/json', ...cors },
        });
      }
    }

    // GET / — simple health check
    if (url.pathname === '/' || url.pathname === '/api' || url.pathname === '/api/') {
      return new Response(JSON.stringify({ service: 'aismush-stats', status: 'ok' }), {
        headers: { 'Content-Type': 'application/json', ...cors },
      });
    }

    return new Response('Not found', { status: 404, headers: cors });
  },
};

async function getGlobalStats(kv) {
  const raw = await kv.get('global_stats', 'json');
  // v2 stats use delta-based reporting; reset if missing version
  if (!raw || !raw.version) {
    return {
      version: 2,
      total_users: 0,
      total_requests: 0,
      total_claude_turns: 0,
      total_deepseek_turns: 0,
      total_routing_savings: 0,
      total_compression_savings: 0,
      total_compressed_tokens: 0,
      total_savings: 0,
      last_updated: null,
    };
  }
  return raw;
}

async function recordReport(kv, report) {
  // Get current totals (resets to zero if upgrading from v1)
  const stats = await getGlobalStats(kv);

  // Reports now contain deltas, so addition is correct
  stats.total_requests += report.requests;
  stats.total_claude_turns += report.claude_turns;
  stats.total_deepseek_turns += report.deepseek_turns;
  stats.total_routing_savings = (stats.total_routing_savings || 0) + report.routing_savings;
  stats.total_compression_savings = (stats.total_compression_savings || 0) + report.compression_savings;
  stats.total_compressed_tokens = (stats.total_compressed_tokens || 0) + report.compressed_tokens;
  stats.total_savings = (stats.total_routing_savings || 0) + (stats.total_compression_savings || 0);
  stats.last_updated = new Date().toISOString();

  // Track unique instances per day
  const today = new Date().toISOString().slice(0, 10);
  const instancesKey = `instances:${today}`;
  const instanceId = report.instance_id || 'unknown';
  const instancesRaw = await kv.get(instancesKey, 'json');
  const instances = instancesRaw || [];
  if (!instances.includes(instanceId)) {
    instances.push(instanceId);
    await kv.put(instancesKey, JSON.stringify(instances), { expirationTtl: 86400 * 7 });
  }
  stats.total_users = Math.max(stats.total_users || 0, instances.length);

  await kv.put('global_stats', JSON.stringify(stats));
}
