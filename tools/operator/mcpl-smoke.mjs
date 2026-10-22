#!/usr/bin/env node
// Exercise ZKGBAI's MCPL endpoint the way an agent host does: handshake,
// policy, tools, the HUD hook, the feed. Prints what comes back.
//
// usage: mcpl-smoke.mjs [ws://127.0.0.1:7460/mcpl] [--pushes N] [--seconds S]
//   Without a URL, the newest instance announced in <tmp>/zkgbai-operator/.
// Needs Node 22 (global WebSocket).

import { readdirSync, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const args = process.argv.slice(2);
let url = args.find((a) => a.startsWith('ws'));
const opt = (name, def) => {
  const i = args.indexOf(name);
  return i >= 0 ? Number(args[i + 1]) : def;
};
const wantPushes = opt('--pushes', 3);
const maxSeconds = opt('--seconds', 120);
// --board <file>: ask for the board section and save the HUD's image there.
const boardFile = (() => { const i = args.indexOf('--board'); return i >= 0 ? args[i + 1] : null; })();

if (!url) {
  const dir = join(tmpdir(), 'zkgbai-operator');
  const notes = readdirSync(dir)
    .filter((f) => f.endsWith('.json'))
    .map((f) => JSON.parse(readFileSync(join(dir, f), 'utf8')))
    .sort((a, b) => b.started - a.started);
  if (!notes.length) throw new Error(`no running instance in ${dir}`);
  url = notes[0].url.replace(/^http/, 'ws') + '/mcpl';
}
console.log(`dialing ${url}`);

const ws = new WebSocket(url);
let nextId = 1;
const pending = new Map();
let pushes = 0;
const done = (code) => {
  try { ws.close(); } catch {}
  setTimeout(() => process.exit(code), 200);
};
const timer = setTimeout(() => {
  console.log(`giving up after ${maxSeconds}s (${pushes} pushes)`);
  done(pushes > 0 ? 0 : 1);
}, maxSeconds * 1000);

function request(method, params) {
  const id = nextId++;
  ws.send(JSON.stringify({ jsonrpc: '2.0', id, method, params }));
  return new Promise((resolve, reject) => pending.set(id, { resolve, reject, method }));
}
function notify(method, params) {
  ws.send(JSON.stringify({ jsonrpc: '2.0', method, params }));
}
const short = (v, n = 400) => {
  const s = JSON.stringify(v);
  return s.length > n ? s.slice(0, n) + '…' : s;
};

ws.addEventListener('message', (ev) => {
  for (const line of String(ev.data).split('\n')) {
    if (!line.trim()) continue;
    const msg = JSON.parse(line);
    if (msg.method === 'push/event') {
      pushes++;
      const p = msg.params;
      const text = p.payload?.content?.map((c) => c.text).join('') ?? '';
      console.log(`\n<< push/event ${p.eventId} tags=${JSON.stringify(p.tags)}\n${text}`);
      ws.send(JSON.stringify({ jsonrpc: '2.0', id: msg.id, result: { accepted: true, inferenceId: `inf-${pushes}` } }));
      if (pushes >= wantPushes) {
        console.log(`\n${pushes} pushes received: ok`);
        clearTimeout(timer);
        done(0);
      }
      continue;
    }
    if (msg.method === 'channels/register') {
      console.log(`<< channels/register ${short(msg.params)}`);
      ws.send(JSON.stringify({ jsonrpc: '2.0', id: msg.id, result: { registered: msg.params.channels.map((c) => c.id) } }));
      // Speak back on the channel as the host would route the agent's prose.
      request('channels/publish', { conversationId: 'c', channelId: 'zk:chat', content: [{ type: 'text', text: 'mcpl smoke: hello spectators\nsecond line' }] })
        .then((r) => console.log(`published: ${short(r)}`));
      continue;
    }
    if (msg.method === 'channels/incoming') {
      for (const m of msg.params.messages ?? []) console.log(`<< chat [${m.author?.name}] ${m.content?.map((c) => c.text).join('')}`);
      ws.send(JSON.stringify({ jsonrpc: '2.0', id: msg.id, result: { results: (msg.params.messages ?? []).map((m) => ({ messageId: m.messageId, accepted: true })) } }));
      continue;
    }
    if (msg.id != null && pending.has(msg.id)) {
      const { resolve, reject, method } = pending.get(msg.id);
      pending.delete(msg.id);
      if (msg.error) {
        console.log(`<< ${method} ERROR ${short(msg.error)}`);
        reject(new Error(msg.error.message));
      } else {
        console.log(`<< ${method}: ${short(msg.result)}`);
        resolve(msg.result);
      }
      continue;
    }
    console.log(`<< ? ${short(msg)}`);
  }
});
ws.addEventListener('error', (e) => { console.error('socket error', e.message ?? e); done(1); });
ws.addEventListener('close', (e) => console.log(`socket closed ${e.code} ${e.reason}`));

ws.addEventListener('open', async () => {
  try {
    const init = await request('initialize', {
      protocolVersion: '2024-11-05',
      capabilities: { experimental: { mcpl: { version: '0.5', pushEvents: true, contextHooks: { beforeInference: true }, inferenceLifecycle: true, featureSets: true } } },
      clientInfo: { name: 'mcpl-smoke', version: '0' },
    });
    const sets = Object.keys(init.capabilities?.experimental?.mcpl?.featureSets ?? {});
    console.log(`feature sets: ${sets.join(', ')}`);
    notify('notifications/initialized');
    await request('featureSets/update', {
      effectiveCapabilities: ['tools', 'pushEvents', 'contextHooks.beforeInference.inject.afterUser', 'inferenceLifecycle', 'channels.register', 'channels.publish', 'channels.incoming'],
      enabled: sets,
      disabled: [],
    });
    const tools = await request('tools/list', {});
    console.log(`tools: ${tools.tools.map((t) => t.name).join(' ')}`);
    const call = async (name, a = {}) => {
      const r = await request('tools/call', { name, arguments: a });
      return r.structuredContent ?? r.content?.[0]?.text;
    };
    await call('status');
    await call('modules');
    await call('mute', { module: 'scouts', off: true });
    await call('feature', { name: 'mexporc', off: true });
    await call('feed', { sitrep_s: 5, batch_ms: 1000 });
    await call('hud', {});
    if (boardFile) {
      await call('hud', { sections: 'all', pin: 'spots' });
      await call('hud', { pin: 'enemies', args: '{"kind":"fighter","visible":true}' });
    }
    notify('inference/lifecycle', { inferenceId: 'inf-0', conversationId: 'c', turnIndex: 0, phase: 'started' });
    const hook = await request('context/beforeInference', { inferenceId: 'inf-0', conversationId: 'c', turnIndex: 0, userMessage: null });
    for (const inj of hook.contextInjections ?? []) {
      for (const block of inj.content ?? []) {
        if (block.type === 'text') console.log(`HUD text ${block.text.length} chars:\n${block.text.slice(0, 1500)}`);
        if (block.type === 'image' && boardFile) {
          const { writeFileSync } = await import('node:fs');
          writeFileSync(boardFile, Buffer.from(block.data, 'base64'));
          console.log(`board image ${block.mimeType} ${block.data.length} base64 chars -> ${boardFile}`);
        }
      }
    }
    notify('inference/lifecycle', { inferenceId: 'inf-0', conversationId: 'c', turnIndex: 0, phase: 'completed', usage: { inputTokens: 1, outputTokens: 1 } });
    await call('note', { text: 'mcpl smoke: waiting for pushes', tag: 'smoke' });
    console.log(`waiting for ${wantPushes} pushes...`);
  } catch (e) {
    console.error('smoke failed:', e.message);
    done(1);
  }
});
