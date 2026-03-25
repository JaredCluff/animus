#!/usr/bin/env swift
// Animus STT helper — wraps macOS SFSpeechRecognizer for file-based transcription.
// Usage: swift transcribe.swift <audio_file_path>
// Prints the transcript to stdout; errors go to stderr.
import Foundation
import Speech

guard CommandLine.arguments.count > 1 else {
    fputs("usage: transcribe.swift <audio_file>\n", stderr)
    exit(1)
}

let fileURL = URL(fileURLWithPath: CommandLine.arguments[1])
let sem = DispatchSemaphore(value: 0)
var exitCode: Int32 = 0

SFSpeechRecognizer.requestAuthorization { auth in
    guard auth == .authorized else {
        fputs("speech recognition not authorized — grant access in System Settings > Privacy > Speech Recognition\n", stderr)
        exitCode = 1
        sem.signal()
        return
    }
    guard let recognizer = SFSpeechRecognizer(), recognizer.isAvailable else {
        fputs("speech recognizer unavailable\n", stderr)
        exitCode = 1
        sem.signal()
        return
    }
    let req = SFSpeechURLRecognitionRequest(url: fileURL)
    req.shouldReportPartialResults = false
    recognizer.recognitionTask(with: req) { result, error in
        if let err = error {
            fputs("recognition error: \(err.localizedDescription)\n", stderr)
            exitCode = 1
            sem.signal()
            return
        }
        if let res = result, res.isFinal {
            print(res.bestTranscription.formattedString)
            sem.signal()
        }
    }
}

sem.wait()
exit(exitCode)
