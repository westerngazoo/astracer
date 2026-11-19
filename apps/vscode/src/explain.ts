import * as vscode from "vscode";

import { Lcw } from "./lcw";

const TIMEOUT_MS = 60_000;

interface Prompt {
  system: string;
  prompt: string;
}

/**
 * The UI's Explain button: `lcw explain-prompt` builds the same prompt the
 * desktop app sends (shared in `lcw-desktop-api`), then the extension calls
 * Ollama (localhost) or Gemini (`$GEMINI_API_KEY`) like the Tauri backend.
 */
export async function explainFunction(
  lcw: Lcw,
  context: unknown,
  provider: string,
): Promise<string> {
  const p = await lcw.json<Prompt>(["explain-prompt", "--json"], {
    stdin: JSON.stringify(context),
  });
  switch (provider) {
    case "ollama":
      return explainOllama(p);
    case "gemini":
      return explainGemini(p);
    default:
      throw new Error(`unknown provider: ${provider} (expected ollama or gemini)`);
  }
}

async function post(url: string, body: unknown, what: string): Promise<Record<string, unknown>> {
  let resp: Response;
  try {
    resp = await fetch(url, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
      signal: AbortSignal.timeout(TIMEOUT_MS),
    });
  } catch (e) {
    throw new Error(`${what} not reachable: ${(e as Error).message}`);
  }
  if (!resp.ok) {
    throw new Error(`${what} error (${resp.status}): ${await resp.text()}`);
  }
  return (await resp.json()) as Record<string, unknown>;
}

async function explainOllama(p: Prompt): Promise<string> {
  const model =
    vscode.workspace.getConfiguration("livewalk").get<string>("ollamaModel")?.trim() ||
    process.env.OLLAMA_MODEL ||
    "qwen2.5-coder:7b";
  const json = await post(
    "http://localhost:11434/api/generate",
    { model, prompt: `${p.system}\n\n${p.prompt}`, stream: false },
    "Ollama at localhost:11434",
  );
  const text = typeof json.response === "string" ? json.response.trim() : "";
  if (!text) {
    throw new Error("Ollama returned an empty response");
  }
  return text;
}

async function explainGemini(p: Prompt): Promise<string> {
  const key = process.env.GEMINI_API_KEY;
  if (!key) {
    throw new Error(
      "GEMINI_API_KEY is not set — export it in the environment the editor was started from",
    );
  }
  const url = `https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-flash:generateContent?key=${encodeURIComponent(key)}`;
  const json = (await post(
    url,
    {
      systemInstruction: { parts: [{ text: p.system }] },
      contents: [{ parts: [{ text: p.prompt }] }],
    },
    "Gemini",
  )) as { candidates?: { content?: { parts?: { text?: string }[] } }[] };
  const text = json.candidates?.[0]?.content?.parts?.[0]?.text?.trim() ?? "";
  if (!text) {
    throw new Error("Gemini returned an empty response");
  }
  return text;
}
