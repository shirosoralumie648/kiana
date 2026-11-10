// 分帧边界的电池：每段之间用 setTimeout 隔开，保证 TS/Rust 都按同样顺序收到。
const chunks = [
  '{"jsonrpc":"2.0","method":"a"}\n',
  '   \n\t\r\n',
  '{"jsonrpc":"2.0","method":"b"}\r\n',
  '{"jsonrpc":"2.0","method":"c"}\r\r\n',
  '{"jsonrpc":"2.0","method":"d"}\r',
  'not json\n',
  '{"hello":"world"}\n',
  '{"jsonrpc":"2.0","method":"e"}\n{"jsonrpc":"2.0","method":"f"}\n',
  '{"jsonrpc":"2.0","method":"trunc',
];
for (const chunk of chunks) {
  process.stdout.write(chunk);
  await new Promise((r) => setTimeout(r, 30));
}
