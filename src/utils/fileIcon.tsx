import type { ReactElement, SVGProps } from 'react'
import {
  FileAudioIcon,
  FileDocIcon,
  FileImageIcon,
  FilePdfIcon,
  FileSheetIcon,
  FileSlideIcon,
  FileTextIcon,
} from '../icons'

const IMAGE_EXTS = ['png', 'jpg', 'jpeg', 'gif', 'bmp', 'webp', 'tiff', 'svg']
const AUDIO_EXTS = ['mp3', 'wav', 'm4a', 'aac', 'flac', 'ogg', 'opus', 'wma']

export function fileTypeIcon(path: string, props?: SVGProps<SVGSVGElement>): ReactElement {
  const dot = path.lastIndexOf('.')
  const ext = dot >= 0 ? path.slice(dot + 1).toLowerCase() : ''
  if (ext === 'pdf') return <FilePdfIcon {...props} />
  if (ext === 'doc' || ext === 'docx' || ext === 'rtf') return <FileDocIcon {...props} />
  if (ext === 'xls' || ext === 'xlsx' || ext === 'csv') return <FileSheetIcon {...props} />
  if (ext === 'ppt' || ext === 'pptx') return <FileSlideIcon {...props} />
  if (IMAGE_EXTS.includes(ext)) return <FileImageIcon {...props} />
  if (AUDIO_EXTS.includes(ext)) return <FileAudioIcon {...props} />
  return <FileTextIcon {...props} />
}
