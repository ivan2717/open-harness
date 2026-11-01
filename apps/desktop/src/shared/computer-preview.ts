/** Ephemeral, local presentation data. Never stored in runs or sent to a model. */
export interface ComputerPreview {
  id: string
  active: boolean
  title?: string
  width?: number
  height?: number
  jpeg?: string
  unavailable?: boolean
  pointer?: { x: number; y: number; pressed: boolean; sequence: number }
}
