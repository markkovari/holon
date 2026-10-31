#!/usr/bin/env node
import http from 'node:http';

const targetUrl = process.argv[2] || 'http://127.0.0.1:8080/ok';
const concurrency = parseInt(process.env.CONCURRENCY || '20', 10);
const durationSec = parseInt(process.env.DURATION || '5', 10);
const tenantHeader = process.env.TENANT_HEADER || ''; // e.g. "X-Tenant-ID"

const parsed = new URL(targetUrl);
const agent = new http.Agent({
  keepAlive: true,
  maxSockets: concurrency * 2,
});

let totalRequests = 0;
let successRequests = 0;
let errorRequests = 0;
const statusCounts = {};
const latenciesMs = [];

let running = true;
const startTime = Date.now();
const endTime = startTime + durationSec * 1000;

function sendRequest(workerId) {
  if (!running && Date.now() >= endTime) return;

  const reqStart = process.hrtime.bigint();
  const headers = {
    'Connection': 'keep-alive',
    'User-Agent': 'holon-bench',
  };
  if (tenantHeader) {
    // Interleave across 10 virtual tenants
    const tenantId = `tenant-${(totalRequests % 10) + 1}`;
    headers[tenantHeader] = tenantId;
  }

  const req = http.request({
    hostname: parsed.hostname,
    port: parsed.port || 80,
    path: parsed.pathname + parsed.search,
    method: 'GET',
    headers,
    agent,
  }, (res) => {
    res.on('data', () => {});
    res.on('end', () => {
      const elapsedNs = process.hrtime.bigint() - reqStart;
      const elapsedMs = Number(elapsedNs) / 1e6;
      latenciesMs.push(elapsedMs);

      totalRequests++;
      statusCounts[res.statusCode] = (statusCounts[res.statusCode] || 0) + 1;
      if (res.statusCode >= 200 && res.statusCode < 400) {
        successRequests++;
      } else {
        errorRequests++;
      }

      if (Date.now() < endTime) {
        sendRequest(workerId);
      }
    });
  });

  req.on('error', (err) => {
    totalRequests++;
    errorRequests++;
    statusCounts['ERR'] = (statusCounts['ERR'] || 0) + 1;
    if (Date.now() < endTime) {
      setTimeout(() => sendRequest(workerId), 50);
    }
  });

  req.end();
}

// Start workers
for (let i = 0; i < concurrency; i++) {
  sendRequest(i);
}

setTimeout(() => {
  running = false;
  // wait 200ms for in-flight requests to complete
  setTimeout(() => {
    const elapsedSec = (Date.now() - startTime) / 1000;
    const rps = (totalRequests / elapsedSec).toFixed(1);

    latenciesMs.sort((a, b) => a - b);
    const p50 = latenciesMs.length ? latenciesMs[Math.floor(latenciesMs.length * 0.50)].toFixed(2) : '0';
    const p90 = latenciesMs.length ? latenciesMs[Math.floor(latenciesMs.length * 0.90)].toFixed(2) : '0';
    const p99 = latenciesMs.length ? latenciesMs[Math.floor(latenciesMs.length * 0.99)].toFixed(2) : '0';

    console.log(`--- Throughput Benchmark Result ---`);
    console.log(`Target:      ${targetUrl}`);
    console.log(`Duration:    ${elapsedSec.toFixed(2)}s, Concurrency: ${concurrency}`);
    console.log(`Requests:    ${totalRequests} total (${rps} req/sec)`);
    console.log(`Success:     ${successRequests}, Errors: ${errorRequests}`);
    console.log(`Latencies:   p50: ${p50}ms | p90: ${p90}ms | p99: ${p99}ms`);
    console.log(`Statuses:    ${JSON.stringify(statusCounts)}`);
    console.log(`-----------------------------------`);
    process.exit(0);
  }, 300);
}, durationSec * 1000);
