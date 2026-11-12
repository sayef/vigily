import { useQuery, useQueryClient } from "@tanstack/react-query";
import { type FormEvent, type ReactNode, useEffect, useState } from "react";
import { Mark, WordmarkText } from "@/components/logo";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { api, ApiError, type Me, UNAUTHORIZED_EVENT } from "@/lib/api";

/** Shows the app once the browser is signed in, and the sign-in form until then. An engine that
 * cannot be reached falls through to the app, whose pages explain that themselves. */
export function AuthGate({ children }: { children: ReactNode }) {
  const client = useQueryClient();
  const me = useQuery({ queryKey: ["me"], queryFn: ({ signal }) => api.me(signal), staleTime: 60_000, retry: 1 });
  useEffect(() => {
    const again = () => void client.invalidateQueries({ queryKey: ["me"] });
    window.addEventListener(UNAUTHORIZED_EVENT, again);
    return () => window.removeEventListener(UNAUTHORIZED_EVENT, again);
  }, [client]);
  if (me.isPending) return null;
  if (me.data && me.data.auth_required && !me.data.authenticated) return <SignIn me={me.data} />;
  return <>{children}</>;
}

function SignIn({ me }: { me: Me }) {
  const client = useQueryClient();
  const [token, setToken] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const signedIn = await api.signIn(token.trim());
      // Anything fetched before signing in was refused; start over with the session.
      client.removeQueries({ predicate: (q) => q.queryKey[0] !== "me" });
      client.setQueryData(["me"], signedIn);
    } catch (err) {
      setError(err instanceof ApiError && err.status === 401 ? "That token is unknown, revoked or expired." : (err as Error).message);
      setBusy(false);
    }
  };
  return (
    <main className="mx-auto flex min-h-dvh max-w-[420px] flex-col justify-center px-4 py-12">
      <div className="flex items-center gap-2" aria-label="vigily">
        <Mark className="h-[19px] w-auto" />
        <WordmarkText className="h-[22px] w-auto" />
      </div>
      <h1 className="mt-8 border-b-[1.5px] border-rule pb-4 text-xl font-bold tracking-[-0.02em]">Sign in</h1>
      <p className="mt-3 text-muted-foreground">
        {me.mode === "local" ? (
          <>
            This engine opens with its local token. Use the link <code className="rounded-sm bg-muted px-1 text-foreground">vigily up</code> printed, or paste the
            token here.
          </>
        ) : (
          "This engine is shared. Paste the admin token or an access token you were given."
        )}
      </p>
      <form onSubmit={submit} className="mt-5 flex flex-col gap-3">
        <label htmlFor="token" className="text-sm font-semibold">
          Token
        </label>
        <Input
          id="token"
          type="password"
          autoComplete="current-password"
          spellCheck={false}
          value={token}
          onChange={(e) => setToken(e.target.value)}
          aria-invalid={error ? true : undefined}
          aria-describedby={error ? "token-error" : undefined}
          className="h-9 font-mono"
          autoFocus
        />
        {error && (
          <p id="token-error" role="alert" className="text-sm text-danger">
            {error}
          </p>
        )}
        <Button type="submit" variant="brand" loading={busy} disabled={!token.trim()} className="self-start">
          Sign in
        </Button>
      </form>
      <p className="mt-6 text-sm text-muted-foreground">The browser keeps the token in a cookie for this site only.</p>
    </main>
  );
}
