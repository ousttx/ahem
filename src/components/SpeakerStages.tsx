import { useTranslation } from "react-i18next";
import type { SpeakersStep } from "../events";
import { clock, runningPercent } from "../present";
import { Progress, useElapsed } from "../ui";

/// Telling voices apart, a bar for each of its two models: where voices speak (the segmentation
/// model does not tell how far it is, so its bar runs without a measure, with the time taken
/// since the step began, `since`, beside it), then whose voices they are, with the share done.
export function SpeakerStages({ step, share, since }: { step: SpeakersStep; share: number | null; since: number }) {
  const { t } = useTranslation();
  const segmenting = step === "segments";
  const elapsed = useElapsed(segmenting, 500, since);
  const voices = segmenting ? null : (share ?? 0);
  return (
    <>
      <div className="file-progress-title">
        <span className="caption">{t("speakers.stage.segments")}</span>
        <span className="caption tabular">{segmenting ? clock(elapsed) : t("speakers.stage.done")}</span>
      </div>
      <Progress value={1} indeterminate={segmenting} label={t("speakers.stage.segments")} />
      <div className="file-progress-title">
        <span className="caption">{t("speakers.stage.voices")}</span>
        {voices !== null && <span className="caption tabular">{runningPercent(voices)}%</span>}
      </div>
      <Progress value={voices ?? 0} label={t("speakers.stage.voices")} />
    </>
  );
}
