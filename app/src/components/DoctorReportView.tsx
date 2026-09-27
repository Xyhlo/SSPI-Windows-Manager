import type { DoctorReport } from "../types"

export function DoctorReportView({ report, applied = false }: { report: DoctorReport; applied?: boolean }) {
  return <details className="doctor" open={report.issues.some(issue => issue.severity === "error")}>
    <summary>Dump doctor: {report.scannedModules} modules checked, {report.repairs.length} {applied ? "repairs applied to the workspace" : "repair candidates"}</summary>
    {!applied && <p className="rail-note">Analysis only. Turn on the dump doctor to apply supported repairs while packaging. Original files stay unchanged.</p>}
    <ul>
      {report.issues.map((issue, index) => <li key={`${issue.path}-${index}`} className={issue.severity === "error" ? "error" : ""}>
        <strong>{issue.path || "Dump"}</strong>: {issue.message}
      </li>)}
    </ul>
  </details>
}
