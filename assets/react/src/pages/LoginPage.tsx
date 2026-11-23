// Sign-in page (/react/login).
//
// Posts to POST /api/v1/auth-manager/jwt/issue — the same endpoint
// assets/js/auth.ts uses — then stores the issued token in the
// session cookie and returns to the dashboard. TOTP and recovery-code
// fields mirror the classic login form: they are only sent when the
// user opens that section and fills one in.

import { useState, type FormEvent } from "react";
import { Link, useNavigate } from "react-router-dom";
import { Alert, Card } from "@theme/components/ui";
import { Button, Input } from "@theme/components/controls";
import { ApiError, login } from "../api";

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
      <Card title="Sign in">
        <p className="mt-1 text-sm text-tertiary">
          Sign in with the account your administrator created for you.
        </p>

        {error !== null && (
          <div className="mt-4">
            <Alert tone="error">{error}</Alert>
          </div>
        )}

        <form onSubmit={handleSubmit} className="mt-5 space-y-4">
          <Input
            id="login-email"
            label="Email"
            type="email"
            autoComplete="username"
            value={email}
            onChange={setEmail}
            placeholder="you@example.com"
          />
          <Input
            id="login-password"
            label="Password"
            type="password"
            autoComplete="current-password"
            value={password}
            onChange={setPassword}
          />

          <div>
            <Button
              color="link"
              size="sm"
              onClick={() => setShowSecondFactor((value) => !value)}
              ariaExpanded={showSecondFactor}
            >
              {showSecondFactor ? "Hide second factor" : "Use an authenticator code"}
            </Button>
            {showSecondFactor && (
              <div className="mt-3 space-y-3">
                {!useRecovery ? (
                  <Input
                    id="login-totp"
                    label="Authenticator code"
                    type="text"
                    inputMode="numeric"
                    autoComplete="one-time-code"
                    value={totp}
                    onChange={setTotp}
                    placeholder="123456"
                  />
                ) : (
                  <Input
                    id="login-recovery"
                    label="Recovery code"
                    type="text"
                    autoComplete="off"
                    value={recovery}
                    onChange={setRecovery}
                  />
                )}
                <Button
                  color="link"
                  size="sm"
                  onClick={() => setUseRecovery((value) => !value)}
                >
                  {useRecovery ? "Use an authenticator code" : "Use a recovery code instead"}
                </Button>
              </div>
            )}
          </div>

          <Button
            type="submit"
            isDisabled={submitting}
            className="w-full"
          >
            {submitting ? "Signing in…" : "Sign in"}
          </Button>
        </form>

        <p className="mt-4 text-sm text-tertiary">
          No account yet? Ask an administrator to create one, or{" "}
          <Link to="/" className="text-accent hover:underline">
            go back to the dashboard
          </Link>
          .
        </p>
      </Card>
    </div>
  );
}
