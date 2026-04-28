import os
import shlex
import tempfile
from pathlib import Path

from terminal_bench.agents.agent_name import AgentName
from terminal_bench.agents.installed_agents.claude_code.claude_code_agent import (
    ClaudeCodeAgent,
)
from terminal_bench.terminal.models import TerminalCommand


class ClaudeFoundryAgent(ClaudeCodeAgent):
    @staticmethod
    def name() -> str:
        return "claude-foundry"

    @property
    def _env(self) -> dict[str, str]:
        env = {
            "ANTHROPIC_API_KEY": os.environ.get(
                "ANTHROPIC_API_KEY", os.environ["ANTHROPIC_FOUNDRY_API_KEY"]
            ),
            "ANTHROPIC_FOUNDRY_API_KEY": os.environ["ANTHROPIC_FOUNDRY_API_KEY"],
            "ANTHROPIC_FOUNDRY_BASE_URL": os.environ[
                "ANTHROPIC_FOUNDRY_BASE_URL"
            ],
            "CLAUDE_CODE_USE_FOUNDRY": os.environ.get(
                "CLAUDE_CODE_USE_FOUNDRY", "1"
            ),
            "FORCE_AUTO_BACKGROUND_TASKS": "1",
            "ENABLE_BACKGROUND_TASKS": "1",
        }
        if self._model_name:
            env["ANTHROPIC_MODEL"] = self._model_name.removeprefix("anthropic/")
        elif "ANTHROPIC_MODEL" in os.environ:
            env["ANTHROPIC_MODEL"] = os.environ["ANTHROPIC_MODEL"]
        if "ANTHROPIC_DEFAULT_SONNET_MODEL" in os.environ:
            env["ANTHROPIC_DEFAULT_SONNET_MODEL"] = os.environ[
                "ANTHROPIC_DEFAULT_SONNET_MODEL"
            ]
        return env

    @property
    def _install_agent_script_path(self) -> Path:
        script = """#!/bin/bash
set -e
apt-get update
apt-get install -y curl
curl -o- https://raw.githubusercontent.com/nvm-sh/nvm/v0.40.2/install.sh | bash
source "$HOME/.nvm/nvm.sh"
nvm install 22
npm install -g @anthropic-ai/claude-code@latest
"""
        temp_file = tempfile.NamedTemporaryFile(mode="w", suffix=".sh", delete=False)
        temp_file.write(script)
        temp_file.close()
        os.chmod(temp_file.name, 0o755)
        return Path(temp_file.name)


class ClaudeFoundryArchivaAgent(ClaudeFoundryAgent):
    @staticmethod
    def name() -> str:
        return "claude-foundry-archiva"

    @property
    def _install_agent_script_path(self) -> Path:
        script = """#!/bin/bash
set -e
apt-get update
apt-get install -y curl
curl -o- https://raw.githubusercontent.com/nvm-sh/nvm/v0.40.2/install.sh | bash
source "$HOME/.nvm/nvm.sh"
nvm install 22
npm install -g @anthropic-ai/claude-code@latest
npm install -g @jalkarna/archiva@latest
"""
        temp_file = tempfile.NamedTemporaryFile(mode="w", suffix=".sh", delete=False)
        temp_file.write(script)
        temp_file.close()
        os.chmod(temp_file.name, 0o755)
        return Path(temp_file.name)

    def _run_agent_commands(self, instruction: str) -> list[TerminalCommand]:
        archiva_instruction = f"""
Use Archiva before and after you work.

Before changing files:
- Run `archiva init --yes` if Archiva is not initialized.
- If `archiva init --yes` is unsupported, run `archiva init`.
- Ask the Archiva MCP `why` tool for the file or area you plan to edit.

After making the change:
- Use the Archiva MCP `write_decision` tool to record what you changed, why, and any rejected alternatives.
- Leave the original benchmark task passing.

Benchmark task:
{instruction}
""".strip()
        escaped_instruction = shlex.quote(archiva_instruction)
        mcp_config = shlex.quote(
            '{"mcpServers":{"archiva":{"command":"archiva","args":["mcp"]}}}'
        )
        return [
            TerminalCommand(
                command=(
                    "claude --verbose --output-format stream-json "
                    "--mcp-config "
                    f"{mcp_config} "
                    "--allowedTools "
                    "Bash Edit Write Read Glob Grep LS "
                    "mcp__archiva__why "
                    "mcp__archiva__write_decision "
                    "mcp__archiva__ghost_check "
                    f"-p {escaped_instruction}"
                ),
                min_timeout_sec=0.0,
                max_timeout_sec=float("inf"),
                block=True,
                append_enter=True,
            ),
        ]
