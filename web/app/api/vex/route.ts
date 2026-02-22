import { NextRequest, NextResponse } from "next/server";
import { executeVexCommand } from "@/lib/vex-client";
import type { VexApiRequest, VexApiResponse } from "@/lib/types";

export const runtime = "nodejs";

export async function POST(req: NextRequest) {
  let body: VexApiRequest;
  try {
    body = await req.json();
  } catch {
    return NextResponse.json(
      { ok: false, error: "Invalid JSON body" } satisfies VexApiResponse,
      { status: 400 }
    );
  }

  const { host, token_id, token_secret, command } = body;
  if (!host || !token_id || !token_secret || !command) {
    return NextResponse.json(
      {
        ok: false,
        error: "Missing required fields: host, token_id, token_secret, command",
      } satisfies VexApiResponse,
      { status: 400 }
    );
  }

  try {
    const response = await executeVexCommand(
      host,
      token_id,
      token_secret,
      command
    );
    return NextResponse.json({ ok: true, response } satisfies VexApiResponse);
  } catch (err) {
    const message =
      err instanceof Error ? err.message : "Unknown error";

    if (message.includes("Authentication failed")) {
      return NextResponse.json(
        { ok: false, error: message } satisfies VexApiResponse,
        { status: 401 }
      );
    }

    if (
      message.includes("ECONNREFUSED") ||
      message.includes("connect") ||
      message.includes("ETIMEDOUT")
    ) {
      return NextResponse.json(
        { ok: false, error: `Connection failed: ${message}` } satisfies VexApiResponse,
        { status: 502 }
      );
    }

    return NextResponse.json(
      { ok: false, error: message } satisfies VexApiResponse,
      { status: 500 }
    );
  }
}
