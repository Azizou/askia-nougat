import { useState } from "react";
import { useI18n } from "../i18n";

export function Faq() {
  const { t } = useI18n();
  const [open, setOpen] = useState<number | null>(0);

  const toggle = (i: number) => setOpen(open === i ? null : i);

  return (
    <div>
      <div className="page-header">
        <h1>{t.faq.title}</h1>
      </div>
      <div className="faq-list">
        {t.faq.sections.map((s, i) => (
          <div key={i} className={`faq-item${open === i ? " open" : ""}`}>
            <button className="faq-question" onClick={() => toggle(i)}>
              <span>{s.q}</span>
              <span className="faq-arrow">{open === i ? "−" : "+"}</span>
            </button>
            {open === i && <div className="faq-answer">{s.a}</div>}
          </div>
        ))}
      </div>
    </div>
  );
}
