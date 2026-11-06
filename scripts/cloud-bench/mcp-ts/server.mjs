// Baseline of the mcp-session scenario (scripts/cloud-bench): the tool of
// crates/cameleer-bench/src/ai.rs written by hand on the official MCP TypeScript SDK, following
// the SDK's stateful Streamable HTTP example (one transport per session).
//
//   node server.mjs serve mcp-session <port> <secs>
import { randomUUID } from 'node:crypto';
import express from 'express';
import { z } from 'zod';
import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js';
import { StreamableHTTPServerTransport } from '@modelcontextprotocol/sdk/server/streamableHttp.js';
import { isInitializeRequest } from '@modelcontextprotocol/sdk/types.js';
import { readFileSync } from 'node:fs';

const [cmd, scenario, portArg, secsArg] = process.argv.slice(2);
if (cmd !== 'serve' || scenario !== 'mcp-session') {
  console.error('usage: node server.mjs serve mcp-session <port> <secs>');
  process.exit(1);
}

function newServer() {
  const server = new McpServer({ name: 'bench', version: '1.0.0' });
  server.registerTool(
    'lookup_order',
    { description: 'Look up an order', inputSchema: { orderId: z.number().int() }, annotations: { readOnlyHint: true } },
    async ({ orderId }) => ({ content: [{ type: 'text', text: JSON.stringify({ orderId, status: 'shipped' }) }] }),
  );
  return server;
}

const transports = new Map();
const app = express();
app.use(express.json());
app.post('/mcp', async (req, res) => {
  const id = req.headers['mcp-session-id'];
  let transport = id ? transports.get(id) : undefined;
  if (!transport) {
    if (id || !isInitializeRequest(req.body)) {
      res.status(400).json({ jsonrpc: '2.0', error: { code: -32000, message: 'Bad Request: no valid session' }, id: null });
      return;
    }
    transport = new StreamableHTTPServerTransport({
      sessionIdGenerator: () => randomUUID(),
      onsessioninitialized: (sid) => transports.set(sid, transport),
    });
    transport.onclose = () => transport.sessionId && transports.delete(transport.sessionId);
    await newServer().connect(transport);
  }
  await transport.handleRequest(req, res, req.body);
});

const sdk = JSON.parse(readFileSync(new URL('./node_modules/@modelcontextprotocol/sdk/package.json', import.meta.url))).version;
const http = app.listen(Number(portArg), '0.0.0.0', () => {
  console.log(`VERSION impl=mcp-typescript-sdk sdk=${sdk} node=${process.version}`);
  console.log('READY');
});
setTimeout(() => { http.close(); process.exit(0); }, Number(secsArg) * 1000);
