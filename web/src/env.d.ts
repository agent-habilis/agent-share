/**
 * File System Access pickers are still missing from some lib.dom builds even
 * though FileSystemDirectoryHandle / FileSystemWritableFileStream are present.
 */
interface Window {
  showDirectoryPicker?(options?: {
    id?: string
    mode?: 'read' | 'readwrite'
    startIn?: FileSystemHandle | string
  }): Promise<FileSystemDirectoryHandle>
  showSaveFilePicker?(options?: {
    suggestedName?: string
  }): Promise<FileSystemFileHandle>
}

declare module '*.css'
