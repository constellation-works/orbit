// Wherever a command has both forms, -h is shorter than --help.

import { resultOf, utf8Bytes } from "../lib/cli-context.mjs";

export const rule = {
  id: "cli.short-help-shorter",
  title: "Short help is shorter than long help",
  severity: "low",
  surface: "cli",
};

export async function check(ctx) {
  const commands = await ctx.discoverCommands();
  const results = [];
  for (const argv of commands) {
    const target = { id: argv.length === 0 ? "orbit" : `orbit ${argv.join(" ")}`, argv };
    const short = await ctx.capture({ argv: [...argv, "-h"] });
    const long = await ctx.capture({ argv: [...argv, "--help"] });
    if (short.exitCode !== 0 || long.exitCode !== 0) {
      results.push(
        resultOf(rule, target, "skipped", {
          selector: "no -h",
          measurements: {
            short_exit: short.exitCode,
            long_exit: long.exitCode,
          },
        }),
      );
      continue;
    }
    const shortBytes = utf8Bytes(short.stdout);
    const longBytes = utf8Bytes(long.stdout);
    const shorter = shortBytes < longBytes;
    results.push(
      resultOf(rule, target, shorter ? "pass" : "fail", {
        selector: shorter ? "" : "-h",
        measurements: { short_bytes: shortBytes, long_bytes: longBytes },
      }),
    );
  }
  return results;
}
