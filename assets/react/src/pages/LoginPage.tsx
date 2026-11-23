// Sign-in page (/react/login).
//
// Posts to POST /api/v1/auth-manager/jwt/issue — the same endpoint
// assets/js/auth.ts uses — then stores the issued token in the
// session cookie and returns to the dashboard. TOTP and recovery-code
// fields mirror the classic login form: they are only sent when the
// user opens that section and fills one in.

import { useState, type FormEvent } from "react";
import { Link, useNavigate } from "react-router-dom";
import { ApiError, login } from "../api";
import { Alert } from "../components/ui";

export default function LoginPage() {
  const navigate = useNavigate();
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [showSecondFactor, setShowSecondFactor] = useState(false);
  const [useRecovery, setUseRecovery] = useState(false);
  const [totp, setTotp] = useState("");
  const [recovery, setRecovery] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  const handleSubmit = (event: FormEvent) => {
    event.preventDefault();
    if (!email.trim() || !password) {
      setError("Email and password are both required.");
      return;
    }
    setError(null);
    setSubmitting(true);
    const input: { email: string; password: string; totp_code?: string; recovery_code?: string } = {
      email: email.trim(),
      password,
    };
    if (showSecondFactor && !useRecovery && totp.trim()) input.totp_code = totp.trim();
    if (showSecondFactor && useRecovery && recovery.trim()) input.recovery_code = recovery.trim();
    void login(input)
      .then(() => navigate("/", { replace: true }))
      .catch((caught: unknown) => {
        setError(
          caught instanceof ApiError
            ? caught.message
            : caught instanceof Error
              ? caught.message
              : "Could not reach the server. Try again.",
        );
      })
      .finally(() => setSubmitting(false));
  };

  return (
    <div className="mx-auto max-w-md">
      <div className="rounded-lg border border-border bg-surface p-6 shadow-md">
        <h2 className="text-xl font-bold">Sign in</h2>
        <p className="mt-1 text-sm text-muted">
          Sign in with the account your administrator created for you.
        </p>

        {error !== null && (
          <div className="mt-4">
            <Alert tone="error">{error}</Alert>
          </div>
        )}

        <form onSubmit={handleSubmit} className="mt-5 space-y-4">
          <div>
            <label htmlFor="login-email" className="mb-1 block text-sm font-medium">
              Email
            </label>
            <input
              id="login-email"
              type="email"
              autoComplete="username"
              value={email}
              onChange={(event) => setEmail(event.target.value)}
              className="w-full rounded-md border border-border bg-surface px-3 py-2 text-sm text-text placeholder:text-faint"
              placeholder="you@example.com"
            />
          </div>
          <div>
            <label htmlFor="login-password" className="mb-1 block text-sm font-medium">
              Password
            </label>
            <input
              id="login-password"
              type="password"
              autoComplete="current-password"
              value={password}
              onChange={(event) => setPassword(event.target.value)}
              className="w-full rounded-md border border-border bg-surface px-3 py-2 text-sm text-text placeholder:text-faint"
            />
          </div>

          <div>
            <button
              type="button"
              onClick={() => setShowSecondFactor((value) => !value)}
              className="text-sm text-accent hover:underline"
              aria-expanded={showSecondFactor}
            >
              {showSecondFactor ? "Hide second factor" : "Use an authenticator code"}
            </button>
            {showSecondFactor && (
              <div className="mt-3 space-y-3">
                {!useRecovery ? (
                  <div>
                    <label htmlFor="login-totp" className="mb-1 block text-sm font-medium">
                      Authenticator code
                    </label>
                    <input
                      id="login-totp"
                      type="text"
                      inputMode="numeric"
                      autoComplete="one-time-code"
                      value={totp}
                      onChange={(event) => setTotp(event.target.value)}
                      className="w-full rounded-md border border-border bg-surface px-3 py-2 text-sm text-text placeholder:text-faint"
                      placeholder="123456"
                    />
                  </div>
                ) : (
                  <div>
                    <label htmlFor="login-recovery" className="mb-1 block text-sm font-medium">
                      Recovery code
                    </label>
                    <input
                      id="login-recovery"
                      type="text"
                      autoComplete="off"
                      value={recovery}
                      onChange={(event) => setRecovery(event.target.value)}
                      className="w-full rounded-md border border-border bg-surface px-3 py-2 text-sm text-text placeholder:text-faint"
                    />
                  </div>
                )}
                <button
                  type="button"
                  onClick={() => setUseRecovery((value) => !value)}
                  className="text-sm text-accent hover:underline"
                >
                  {useRecovery ? "Use an authenticator code" : "Use a recovery code instead"}
                </button>
              </div>
            )}
          </div>

          <button
            type="submit"
            disabled={submitting}
            className="w-full rounded-md bg-accent px-4 py-2 text-sm font-semibold text-accent-contrast hover:bg-accent-hover disabled:opacity-50"
          >
            {submitting ? "Signing in…" : "Sign in"}
          </button>
        </form>

        <p className="mt-4 text-sm text-muted">
          No account yet? Ask an administrator to create one, or{" "}
          <Link to="/" className="text-accent hover:underline">
            go back to the dashboard
          </Link>
          .
        </p>
      </div>
    </div>
  );
}
