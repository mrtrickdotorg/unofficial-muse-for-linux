// Stand in for the WebKit message handler: record every command and answer
// it the way the native side would, through window.museReply. Shared by the
// page suites, so every page is tested against the same channel.
// `delayMs` holds an answer back, so a suite can watch a control while its
// write is still in flight -- the window in which the native card refuses a
// second press.
export function installChannel(
  answer: (command: string) => Record<string, unknown>,
  seen?: (request: Record<string, unknown>) => void,
  delayMs = 0,
): string[] {
  const commands: string[] = [];
  const reply = (
    window as unknown as { museReply?: (id: number, body: unknown) => void }
  ).museReply;
  (window as unknown as { webkit?: unknown }).webkit = {
    messageHandlers: {
      muse: {
        postMessage(body: string) {
          const request = JSON.parse(body) as { id: number; cmd: string };
          const { id, cmd } = request;
          commands.push(cmd);
          seen?.(request);
          const answerFor = answer(cmd);
          setTimeout(() => reply?.(id, answerFor), delayMs);
        },
      },
    },
  };
  return commands;
}
