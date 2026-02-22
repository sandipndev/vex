"use client";

import { useState } from "react";
import { useConnection } from "./connection-provider";

export function ConnectForm() {
  const { connect, loading, error } = useConnection();
  const [host, setHost] = useState("");
  const [pairing, setPairing] = useState("");

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    await connect({ host, pairing: pairing.trim() });
  };

  return (
    <form onSubmit={handleSubmit} className="w-full max-w-md space-y-4">
      <div>
        <label
          htmlFor="host"
          className="block text-sm font-medium text-neutral-400 mb-1"
        >
          Host
        </label>
        <input
          data-cy="host-input"
          id="host"
          type="text"
          value={host}
          onChange={(e) => setHost(e.target.value)}
          placeholder="myserver.com:7423"
          required
          className="w-full px-3 py-2 bg-neutral-900 border border-neutral-700 rounded text-white placeholder-neutral-600 focus:outline-none focus:border-white transition-colors"
        />
      </div>

      <div>
        <label
          htmlFor="pairing"
          className="block text-sm font-medium text-neutral-400 mb-1"
        >
          Pairing String
        </label>
        <input
          data-cy="pairing-input"
          id="pairing"
          type="password"
          value={pairing}
          onChange={(e) => setPairing(e.target.value)}
          placeholder="tok_a1b2c3:64hexsecret..."
          required
          className="w-full px-3 py-2 bg-neutral-900 border border-neutral-700 rounded text-white placeholder-neutral-600 focus:outline-none focus:border-white transition-colors"
        />
      </div>

      {error && (
        <p data-cy="error-message" className="text-red-400 text-sm">
          {error}
        </p>
      )}

      <button
        data-cy="connect-button"
        type="submit"
        disabled={loading}
        className="w-full py-2 px-4 bg-white text-black font-semibold rounded hover:bg-neutral-200 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
      >
        {loading ? "Connecting..." : "Connect"}
      </button>
    </form>
  );
}
