import { useState } from "react";
import { copyText } from "../lib/conn";

export interface CaWizardProps {
  caPath: string | null;
  onDone: () => void;
}

const STEPS: { title: string; body: React.ReactNode }[] = [
  {
    title: "Export the CA",
    body: (
      <>
        <p>Aether generated a local Certificate Authority on first start.</p>
        <pre>aether ca export --out aether-ca.pem</pre>
      </>
    ),
  },
  {
    title: "Trust it (once per machine)",
    body: (
      <>
        <p>Import <code>aether-ca.pem</code> as a trusted root:</p>
        <ul>
          <li><b>Firefox:</b> Settings → Privacy &amp; Security → Certificates → View Certificates → Authorities → Import</li>
          <li><b>Chrome/Edge (Linux):</b> Settings → Privacy &amp; Security → Security → Manage Certificates → Authorities → Import</li>
          <li><b>macOS:</b> double-click the .pem → add to <i>System</i> keychain → set “Always Trust”</li>
          <li><b>Windows:</b> double-click the .pem → Install Certificate → Local Machine → “Trusted Root Certification Authorities”</li>
        </ul>
        <p className="ca-note">
          This CA is generated locally, never leaves this machine, and signs leaf
          certificates only for this proxy session. Remove it any time from your
          browser’s certificate settings.
        </p>
      </>
    ),
  },
  {
    title: "Point your client at the proxy",
    body: (
      <>
        <pre>curl -x http://127.0.0.1:8888 https://example.com/</pre>
        <p>
          Or set the system/browser proxy to <code>127.0.0.1:8888</code>. The
          console shows a live TLS chip per flow once HTTPS interception works.
        </p>
      </>
    ),
  },
];

/**
 * First-run CA setup guide. Zero-config is the goal; until the OS-level
 * install is automatable safely, the wizard makes the ritual a 60-second,
 * no-docs path.
 */
export function CaWizard(props: CaWizardProps) {
  const [step, setStep] = useState(0);
  const [copied, setCopied] = useState(false);

  const copyExport = async () => {
    if (await copyText("aether ca export --out aether-ca.pem")) {
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    }
  };

  return (
    <div className="ca-wizard" data-testid="ca-wizard">
      <div className="ca-steps">
        {STEPS.map((s, i) => (
          <button
            key={s.title}
            className={`ca-dot ${i === step ? "active" : ""} ${i < step ? "done" : ""}`}
            onClick={() => setStep(i)}
            aria-label={`Step ${i + 1}: ${s.title}`}
          >
            {i < step ? "✓" : i + 1}
          </button>
        ))}
        <span className="ca-title">{STEPS[step].title}</span>
      </div>

      <div className="ca-body">{STEPS[step].body}</div>

      <div className="ca-actions">
        {step === 0 && props.caPath !== null && (
          <span className="ca-path">
            CA on disk: <code>{props.caPath}</code>
            <button className="btn tiny" onClick={copyExport}>
              {copied ? "Copied ✓" : "Copy command"}
            </button>
          </span>
        )}
        <button className="btn tiny" onClick={props.onDone}>Hide</button>
        {step < STEPS.length - 1 ? (
          <button className="btn primary tiny" onClick={() => setStep(step + 1)}>
            Next →
          </button>
        ) : (
          <button className="btn primary tiny" onClick={props.onDone}>
            Done — take me to the console
          </button>
        )}
      </div>
    </div>
  );
}
