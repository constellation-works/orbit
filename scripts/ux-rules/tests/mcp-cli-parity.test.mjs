import assert from "node:assert/strict";
import test from "node:test";

import { check } from "../cli/mcp-cli-parity.mjs";

const TEMPLATE_DESCRIPTION =
  "Task template: `{ title, description?, acceptance_criteria?, task_type?, tags?, priority?, complexity?, crew?, status? }`. Required.";

const SCHEDULE_DESCRIPTION =
  "Schedule object: exactly one of `{ cron: string }` `{ every_minutes: number }`, `{ deliveries_landed: { branch } }`. Required.";

function autoTaskAddHelp({ scheduleFlags = true, titleRequired = true, extraRequired = "" } = {}) {
  const required = titleRequired ? " --title <TITLE>" : "";
  const extra = extraRequired ? ` ${extraRequired}` : "";
  const schedule = scheduleFlags
    ? `
      --cron <CRON>
          5-field cron expression (mutually exclusive with \`--every-minutes\`)

      --every-minutes <EVERY_MINUTES>
          Interval in minutes (mutually exclusive with \`--cron\`)

      --deliveries-landed <DELIVERIES_LANDED>
          Delivery trigger JSON
`
    : "";
  const title = `
      --title <TITLE>
          Title of each minted task
`;
  return `Create a new auto-task definition

Usage: orbit auto-task add [OPTIONS] --name <NAME>${required}${extra}

Options:
      --name <NAME>
          Unique definition name
${schedule}${title}
      --body <BODY>
          Body / instruction of each minted task

          [default: ""]

  -h, --help
          Print help

Global options (see 'orbit --help' for descriptions):
  --root <ROOT>
  --workspace <SELECTOR>
  --json
`;
}

function autoTaskAddTool({ templateDescription = TEMPLATE_DESCRIPTION, extraRequired = [] } = {}) {
  return {
    name: "orbit_auto_task_add",
    inputSchema: {
      type: "object",
      properties: {
        name: { type: "string", description: "Unique definition name. Required." },
        schedule: { type: "object", description: SCHEDULE_DESCRIPTION },
        template: { type: "object", description: templateDescription },
        description: { type: "string", description: "Human description of the recurring chore." },
        workspace: { type: "string", description: "Workspace selector." },
      },
      required: ["name", "schedule", "template", "workspace", ...extraRequired],
    },
  };
}

function contextFor(tools, helpByCommand) {
  const commands = Object.keys(helpByCommand).map((command) => command.split(" "));
  return {
    async listMcpTools() {
      return tools;
    },
    async discoverCommands() {
      return commands;
    },
    async capture({ argv }) {
      const command = argv.slice(0, -1).join(" ");
      const help = helpByCommand[command];
      if (!help) {
        return { exitCode: 1, stdout: "", stderr: "missing help" };
      }
      return { exitCode: 0, stdout: help, stderr: "" };
    },
  };
}

function resultFor(results, name) {
  const found = results.find((result) => result.target.id === name);
  assert.ok(found, `missing result for ${name}`);
  return found;
}

test("auto-task add schedule and template count as the same required inputs", async () => {
  const results = await check(
    contextFor([autoTaskAddTool()], { "auto-task add": autoTaskAddHelp() }),
  );
  const result = resultFor(results, "orbit_auto_task_add");
  assert.equal(result.status, "pass");
  assert.deepEqual(result.measurements.missing_on_cli, []);
  assert.deepEqual(result.measurements.missing_on_mcp, []);
  assert.deepEqual(result.measurements.cli_required, ["name", "schedule", "title"]);
  assert.deepEqual(result.measurements.mcp_required, ["name", "schedule", "template", "workspace"]);
  assert.deepEqual(result.measurements.composed_equivalences, {
    mcp: ["template"],
    cli: ["title"],
  });
});

test("auto-task add still fails when the schedule flags or template title are not equivalent", async () => {
  const missingSchedule = await check(
    contextFor([autoTaskAddTool()], {
      "auto-task add": autoTaskAddHelp({ scheduleFlags: false }),
    }),
  );
  assert.deepEqual(resultFor(missingSchedule, "orbit_auto_task_add").measurements.missing_on_cli, [
    "schedule",
  ]);

  const optionalTitle = await check(
    contextFor(
      [autoTaskAddTool({ templateDescription: "Task template: `{ title?, description? }`." })],
      { "auto-task add": autoTaskAddHelp() },
    ),
  );
  const optional = resultFor(optionalTitle, "orbit_auto_task_add");
  assert.equal(optional.status, "fail");
  assert.deepEqual(optional.measurements.missing_on_cli, ["template"]);
  assert.deepEqual(optional.measurements.missing_on_mcp, ["title"]);
});

test("auto-task add still reports a required input the other side does not have", async () => {
  const cliOnly = await check(
    contextFor([autoTaskAddTool()], {
      "auto-task add": autoTaskAddHelp({ extraRequired: "--owner <OWNER>" }),
    }),
  );
  assert.deepEqual(resultFor(cliOnly, "orbit_auto_task_add").measurements.missing_on_mcp, [
    "owner",
  ]);

  const mcpOnly = await check(
    contextFor([autoTaskAddTool({ extraRequired: ["owner"] })], {
      "auto-task add": autoTaskAddHelp(),
    }),
  );
  assert.deepEqual(resultFor(mcpOnly, "orbit_auto_task_add").measurements.missing_on_cli, [
    "owner",
  ]);
});

test("another command keeps a direct required-name comparison", async () => {
  const help = `Usage: orbit widget add [OPTIONS] --name <NAME>

Options:
      --name <NAME>
          Name

      --cron <CRON>
          5-field cron expression (mutually exclusive with \`--every-minutes\`)

      --every-minutes <EVERY_MINUTES>
          Interval in minutes

      --deliveries-landed <DELIVERIES_LANDED>
          Delivery trigger JSON

Global options:
  --workspace <SELECTOR>
`;
  const results = await check(
    contextFor(
      [
        {
          name: "orbit_widget_add",
          inputSchema: {
            type: "object",
            properties: {
              name: { type: "string" },
              schedule: { type: "object", description: SCHEDULE_DESCRIPTION },
              template: { type: "object", description: TEMPLATE_DESCRIPTION },
              workspace: { type: "string" },
            },
            required: ["name", "schedule", "template", "workspace"],
          },
        },
      ],
      { "widget add": help },
    ),
  );
  const result = resultFor(results, "orbit_widget_add");
  assert.equal(result.status, "fail");
  assert.deepEqual(result.measurements.missing_on_cli, ["schedule", "template"]);
  assert.deepEqual(result.measurements.missing_on_mcp, []);
  assert.equal(result.measurements.composed_equivalences, undefined);
});

test("decision raise maps --task to task_id and keeps the required --options flag", async () => {
  const help = `Raise a decision for a task

Usage: orbit decision raise [OPTIONS] --task <TASK_ID> --question <QUESTION> --options <OPTIONS>

Options:
      --task <TASK_ID>
          Task to receive the decision

      --question <QUESTION>
          Decision question

      --options <OPTIONS>
          Decision options

Global options:
  --workspace <SELECTOR>
`;
  const required = ["options", "question", "task_id", "workspace"];
  const tool = {
    name: "orbit_decision_raise",
    inputSchema: {
      type: "object",
      properties: {
        options: { type: "array" },
        question: { type: "string" },
        task_id: { type: "string" },
        workspace: { type: "string" },
      },
      required,
    },
  };
  const result = resultFor(
    await check(contextFor([tool], { "decision raise": help })),
    "orbit_decision_raise",
  );
  assert.equal(result.status, "pass");
  assert.deepEqual(result.measurements.cli_required, ["options", "question", "task_id"]);
  assert.deepEqual(result.measurements.missing_on_cli, []);
  assert.deepEqual(result.measurements.missing_on_mcp, []);

  const withMissingField = {
    ...tool,
    inputSchema: {
      ...tool.inputSchema,
      properties: { ...tool.inputSchema.properties, owner: { type: "string" } },
      required: [...required, "owner"],
    },
  };
  const mismatch = resultFor(
    await check(contextFor([withMissingField], { "decision raise": help })),
    "orbit_decision_raise",
  );
  assert.equal(mismatch.status, "fail");
  assert.deepEqual(mismatch.measurements.missing_on_cli, ["owner"]);
});

test("auto-task update does not treat its optional schedule flags as required", async () => {
  const help = `Usage: orbit auto-task update [OPTIONS] --name <NAME>

Options:
      --name <NAME>
          Definition name

      --cron <CRON>
          5-field cron expression (mutually exclusive with \`--every-minutes\`)

      --every-minutes <EVERY_MINUTES>
          Interval in minutes

      --deliveries-landed <DELIVERIES_LANDED>
          Delivery trigger JSON

      --title <TITLE>
          New task title

Global options:
  --workspace <SELECTOR>
`;
  const results = await check(
    contextFor(
      [
        {
          name: "orbit_auto_task_update",
          inputSchema: {
            type: "object",
            properties: {
              name: { type: "string" },
              schedule: { type: "object", description: SCHEDULE_DESCRIPTION },
              template: { type: "object", description: TEMPLATE_DESCRIPTION },
              workspace: { type: "string" },
            },
            required: ["name", "workspace"],
          },
        },
      ],
      { "auto-task update": help },
    ),
  );
  const result = resultFor(results, "orbit_auto_task_update");
  assert.equal(result.status, "pass");
  assert.deepEqual(result.measurements.cli_required, ["name"]);
  assert.deepEqual(result.measurements.missing_on_cli, []);
  assert.deepEqual(result.measurements.missing_on_mcp, []);
});
