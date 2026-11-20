// Stand in for the WebKit message handler: record every command and answer
// it the way the native side would, through window.museReply. Shared by the
// page suites, so every page is tested against the same channel.
export function installChannel(
  answer: (command: string) => Record<string, unknown>,
): string[] {
  const commands: string[] = [];
  const reply = (
    window as unknown as { museReply?: (id: number, body: unknown) => void }
  ).museReply;
  (window as unknown as { webkit?: unknown }).webkit = {
    messageHandlers: {
      muse: {
        postMessage(body: string) {
          const { id, cmd } = JSON.parse(body) as { id: number; cmd: string };
          commands.push(cmd);
          const answerFor = answer(cmd);
          setTimeout(() => reply?.(id, answerFor), 0);
        },
      },
    },
  };
  return commands;
}
