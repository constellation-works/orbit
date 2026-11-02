// Wherever a command has both forms, -h carries no content that --help lacks.
//
// Content, not bytes: clap's short layout pads options into aligned columns
// while the long layout puts each description on its own indented line, so a
// short help can be larger in bytes and still say less. The rule compares the
// words of each form after dropping clap's help-flag footer, which is the one
// place the two forms differ by design.

import { resultOf } from "../lib/cli-context.mjs";

export const rule = {
  id: "cli.short-help-shorter",
  title: "Short help carries no content beyond long help",
  severity: "low",
  surface: "cli",
};

// clap's `-h, --help` description differs between the forms: "(see more with
// '--help')" in -h and "(see a summary with '-h')" in --help.
const HELP_FOOTERS = [/\(see more with '--help'\)/g, /\(see a summary with '-h'\)/g];

const MISSING_SAMPLE = 10;

/** Lowercased words of a help text, without clap's help-flag footer. */
export function helpWords(text) {
  let body = text;
  for (const footer of HELP_FOOTERS) {
    body = body.replace(footer, " ");
  }
  return body.toLowerCase().match(/[\p{L}\p{N}]+/gu) ?? [];
}

function wordCounts(words) {
  const counts = new Map();
  for (const word of words) {
    counts.set(word, (counts.get(word) ?? 0) + 1);
  }
  return counts;
}

/**
 * Compare the content of a short and a long help text as word multisets.
 * Returns the words -h has beyond --help (with repeats) and how many words
 * --help adds beyond -h.
 */
export function compareHelpContent(shortText, longText) {
  const shortWords = helpWords(shortText);
  const longWords = helpWords(longText);
  const longCounts = wordCounts(longWords);
  const missing = [];
  for (const [word, count] of wordCounts(shortWords)) {
    const extra = count - (longCounts.get(word) ?? 0);
    for (let i = 0; i < extra; i += 1) {
      missing.push(word);
    }
  }
  return {
    shortWords: shortWords.length,
    longWords: longWords.length,
    missing,
    added: longWords.length - (shortWords.length - missing.length),
  };
}

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
          measurements: {
            reason: "no -h",
            short_exit: short.exitCode,
            long_exit: long.exitCode,
          },
        }),
      );
      continue;
    }
    const content = compareHelpContent(short.stdout, long.stdout);
    const measurements = {
      short_words: content.shortWords,
      long_words: content.longWords,
      missing_from_long: content.missing.length,
      added_by_long: content.added,
    };
    if (content.missing.length > 0) {
      results.push(
        resultOf(rule, target, "fail", {
          measurements: {
            ...measurements,
            missing_sample: [...new Set(content.missing)].slice(0, MISSING_SAMPLE).join(" "),
          },
        }),
      );
    } else if (content.added === 0) {
      results.push(resultOf(rule, target, "skipped", { measurements: { reason: "no long help", ...measurements } }));
    } else {
      results.push(resultOf(rule, target, "pass", { measurements }));
    }
  }
  return results;
}
