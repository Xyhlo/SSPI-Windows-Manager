// Display aliases only: archive passwords, filenames on disk and source IDs stay intact.
export const displayText = (text: string) => text.replace(/\[?dlps(?:game)?(?:\.com)?\]?/gi, "Global").replace(/Global (PS[45]) Static/gi, "Global $1")
