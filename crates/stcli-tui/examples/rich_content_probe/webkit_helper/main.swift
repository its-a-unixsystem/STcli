import AppKit
import CryptoKit
import Darwin
import Foundation
import PDFKit
import WebKit

private let maxHTML = 256 * 1024
private let maxAsset = 1024 * 1024
private let maxAssetPixels = 1_048_576
private let maxAssetsTotal = 4 * 1024 * 1024
private let maxWidth = 1600
private let maxHeight = 4096
private let maxPNG = 32 * 1024 * 1024
private let assetDigest = "f331033487acbbe3714bda038a21e91ef640c9f224748bb7078b9bd5e2eb4817"
private let allowedKeys: Set<String> = ["id", "fixture", "width", "document_base64", "observe", "assets_base64"]

private struct Request {
    let id: UInt64
    let fixture: String
    let width: Int
    let document: Data
    let observe: Bool
    let assets: [Data]
}

private func compactError(_ error: Error) -> String {
    String(describing: error)
        .replacingOccurrences(of: "\n", with: " ")
        .replacingOccurrences(of: "\r", with: " ")
        .prefix(512)
        .description
}

private func emit(_ value: [String: Any]) {
    guard JSONSerialization.isValidJSONObject(value),
          let data = try? JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]) else {
        print("{\"error\":\"failed to encode response\"}")
        fflush(stdout)
        return
    }
    FileHandle.standardOutput.write(data)
    FileHandle.standardOutput.write(Data([0x0a]))
    fflush(stdout)
}

private func pngDimensions(_ data: Data, pixelLimit: Int = maxAssetPixels) throws -> (Int, Int) {
    let signature = Data([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])
    guard data.count >= 24, data.prefix(8) == signature,
          data.subdata(in: 12..<16) == Data("IHDR".utf8) else {
        throw NSError(domain: "probe", code: 1, userInfo: [NSLocalizedDescriptionKey: "input is not a PNG"])
    }
    let width = data.subdata(in: 16..<20).reduce(0) { ($0 << 8) | Int($1) }
    let height = data.subdata(in: 20..<24).reduce(0) { ($0 << 8) | Int($1) }
    guard width > 0, height > 0, width <= pixelLimit / height else {
        throw NSError(domain: "probe", code: 2, userInfo: [NSLocalizedDescriptionKey: "PNG exceeds decoded-pixel ceiling"])
    }
    return (width, height)
}

private func parseRequest(_ line: String, fixturesDirectory: URL) throws -> Request {
    guard let data = line.data(using: .utf8),
          let object = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
        throw NSError(domain: "probe", code: 3, userInfo: [NSLocalizedDescriptionKey: "request must be a JSON object"])
    }
    let unknown = Set(object.keys).subtracting(allowedKeys)
    guard unknown.isEmpty else {
        throw NSError(domain: "probe", code: 4, userInfo: [NSLocalizedDescriptionKey: "request contains unknown keys"])
    }
    guard let idNumber = object["id"] as? NSNumber,
          CFGetTypeID(idNumber) != CFBooleanGetTypeID(),
          idNumber.doubleValue >= 0,
          idNumber.doubleValue.rounded(.towardZero) == idNumber.doubleValue else {
        throw NSError(domain: "probe", code: 5, userInfo: [NSLocalizedDescriptionKey: "request id must be a u64"])
    }
    let id = idNumber.uint64Value
    guard idNumber.stringValue == String(id) else {
        throw NSError(domain: "probe", code: 6, userInfo: [NSLocalizedDescriptionKey: "request id must be a u64"])
    }
    guard let fixture = object["fixture"] as? String, ["card", "columns", "hostile", "document"].contains(fixture) else {
        throw NSError(domain: "probe", code: 7, userInfo: [NSLocalizedDescriptionKey: "unknown fixture"])
    }
    guard let widthNumber = object["width"] as? NSNumber,
          CFGetTypeID(widthNumber) != CFBooleanGetTypeID() else {
        throw NSError(domain: "probe", code: 8, userInfo: [NSLocalizedDescriptionKey: "width must be an integer"])
    }
    let width = widthNumber.intValue
    guard widthNumber.doubleValue == Double(width), (160...maxWidth).contains(width) else {
        throw NSError(domain: "probe", code: 9, userInfo: [NSLocalizedDescriptionKey: "width must be between 160 and 1600"])
    }

    let document: Data
    if let encoded = object["document_base64"] as? String {
        guard let decoded = Data(base64Encoded: encoded) else {
            throw NSError(domain: "probe", code: 10, userInfo: [NSLocalizedDescriptionKey: "document_base64 is invalid"])
        }
        document = decoded
    } else {
        guard fixture == "card" || fixture == "columns" else {
            throw NSError(domain: "probe", code: 11, userInfo: [NSLocalizedDescriptionKey: "custom fixture requires document_base64"])
        }
        document = try Data(contentsOf: fixturesDirectory.appendingPathComponent("\(fixture).html"))
    }
    guard document.count <= maxHTML else {
        throw NSError(domain: "probe", code: 12, userInfo: [NSLocalizedDescriptionKey: "document exceeds HTML ceiling"])
    }

    let encodedAssets = object["assets_base64"] as? [String] ?? []
    var assets: [Data] = []
    var aggregate = 0
    for encoded in encodedAssets {
        guard let asset = Data(base64Encoded: encoded) else {
            throw NSError(domain: "probe", code: 13, userInfo: [NSLocalizedDescriptionKey: "asset base64 is invalid"])
        }
        guard asset.count <= maxAsset else {
            throw NSError(domain: "probe", code: 14, userInfo: [NSLocalizedDescriptionKey: "asset exceeds ceiling"])
        }
        aggregate += asset.count
        guard aggregate <= maxAssetsTotal else {
            throw NSError(domain: "probe", code: 15, userInfo: [NSLocalizedDescriptionKey: "aggregate assets exceed ceiling"])
        }
        _ = try pngDimensions(asset)
        assets.append(asset)
    }
    return Request(
        id: id,
        fixture: fixture,
        width: width,
        document: document,
        observe: object["observe"] as? Bool ?? false,
        assets: assets
    )
}

private final class DocumentHandler: NSObject, WKURLSchemeHandler {
    let document: Data
    let asset: Data?
    private(set) var requestedURLs: [String] = []

    init(document: Data, asset: Data?) {
        self.document = document
        self.asset = asset
    }

    func webView(_ webView: WKWebView, start urlSchemeTask: WKURLSchemeTask) {
        guard let url = urlSchemeTask.request.url else {
            urlSchemeTask.didFailWithError(NSError(domain: "probe", code: 16))
            return
        }
        requestedURLs.append(url.absoluteString)
        let body: Data
        let mime: String
        if url.path == "/assets/emblem.png", let asset {
            body = asset
            mime = "image/png"
        } else if url.host == "render" {
            body = document
            mime = "text/html"
        } else {
            urlSchemeTask.didFailWithError(NSError(domain: "probe", code: 17))
            return
        }
        let response = URLResponse(url: url, mimeType: mime, expectedContentLength: body.count, textEncodingName: mime == "text/html" ? "utf-8" : nil)
        urlSchemeTask.didReceive(response)
        urlSchemeTask.didReceive(body)
        urlSchemeTask.didFinish()
    }

    func webView(_ webView: WKWebView, stop urlSchemeTask: WKURLSchemeTask) {}
}

private final class Renderer: NSObject, WKNavigationDelegate, WKUIDelegate {
    let request: Request
    let ruleList: WKContentRuleList
    let asset: Data?
    let completion: ([String: Any]) -> Void
    let handler: DocumentHandler
    private(set) var deniedNavigations: [String] = []
    private(set) var webContentTerminated = false
    private(set) var webView: WKWebView!
    private var window: NSWindow!
    private var finished = false
    private var mainNavigationFinished = false

    init(request: Request, ruleList: WKContentRuleList, fixtureAsset: Data?, completion: @escaping ([String: Any]) -> Void) {
        self.request = request
        self.ruleList = ruleList
        self.asset = request.assets.first ?? fixtureAsset
        self.completion = completion
        self.handler = DocumentHandler(document: request.document, asset: request.assets.first ?? fixtureAsset)
    }

    func start() {
        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = .nonPersistent()
        configuration.defaultWebpagePreferences.allowsContentJavaScript = false
        configuration.userContentController.add(ruleList)
        configuration.setURLSchemeHandler(handler, forURLScheme: "stcli-probe")
        webView = WKWebView(frame: CGRect(x: 0, y: 0, width: request.width, height: 600), configuration: configuration)
        webView.navigationDelegate = self
        webView.uiDelegate = self
        window = NSWindow(
            contentRect: webView.frame,
            styleMask: [.borderless],
            backing: .buffered,
            defer: false
        )
        window.contentView = webView
        window.orderBack(nil)
        webView.load(URLRequest(url: URL(string: "stcli-probe://render/\(request.fixture).html")!))
        pollReadiness()
    }

    private func pollReadiness() {
        guard !finished else { return }
        guard mainNavigationFinished else {
            DispatchQueue.main.asyncAfter(deadline: .now() + .milliseconds(50)) { self.pollReadiness() }
            return
        }
        webView.evaluateJavaScript("document.readyState", in: nil, in: .defaultClient) { [weak self] result in
            guard let self else { return }
            switch result {
            case .failure(let error):
                self.fail("readiness failed: \(compactError(error))")
            case .success(let value) where value as? String == "complete":
                self.measureLayout()
            default:
                DispatchQueue.main.asyncAfter(deadline: .now() + .milliseconds(50)) { self.pollReadiness() }
            }
        }
    }

    private func measureLayout() {
        webView.createPDF(configuration: WKPDFConfiguration()) { [weak self] result in
            guard let self else { return }
            switch result {
            case .failure(let error):
                self.fail("layout measurement failed: \(compactError(error))")
            case .success(let data):
                guard let document = PDFDocument(data: data), let page = document.page(at: 0) else {
                    self.fail("layout measurement failed: invalid PDF")
                    return
                }
                let measured = Int(ceil(page.bounds(for: .mediaBox).height))
                guard measured > 0 else {
                    self.fail("layout measurement failed: empty PDF")
                    return
                }
                guard measured <= maxHeight else {
                    self.fail("output exceeds 4096 px ceiling")
                    return
                }
                self.snapshot(height: measured)
            }
        }
    }

    private func snapshot(height: Int) {
        let frame = CGRect(x: 0, y: 0, width: request.width, height: height)
        window.setContentSize(frame.size)
        webView.frame = frame
        webView.layoutSubtreeIfNeeded()
        let configuration = WKSnapshotConfiguration()
        configuration.rect = frame
        configuration.snapshotWidth = NSNumber(value: request.width)
        configuration.afterScreenUpdates = true
        webView.takeSnapshot(with: configuration) { [weak self] image, error in
            guard let self else { return }
            if let error {
                self.fail("snapshot failed: \(compactError(error))")
                return
            }
            guard let image, let tiff = image.tiffRepresentation,
                  let bitmap = NSBitmapImageRep(data: tiff),
                  let png = bitmap.representation(using: .png, properties: [:]) else {
                self.fail("snapshot failed: PNG encoding failed")
                return
            }
            guard png.count <= maxPNG else {
                self.fail("snapshot exceeds 32 MiB ceiling")
                return
            }
            let scale = self.window.backingScaleFactor
            guard let dimensions = try? pngDimensions(png, pixelLimit: Int.max),
                  dimensions.0 == Int((Double(self.request.width) * scale).rounded()),
                  dimensions.1 == Int((Double(height) * scale).rounded()) else {
                self.fail("snapshot dimensions do not match layout")
                return
            }
            self.collectObservations(png: png, height: height, scale: scale)
        }
    }

    private func collectObservations(png: Data, height: Int, scale: Double) {
        let script = """
        (() => {
          let cookie = "";
          try { cookie = document.cookie; } catch (_) {}
          const frames = Array.from(document.querySelectorAll('iframe'));
          return {
            marker_state: document.documentElement.dataset.state || document.body?.dataset.state || null,
            nested_marker_count: document.querySelectorAll('#nested-marker').length,
            external_frame_count: frames.filter((frame) => {
              const value = frame.getAttribute('src') || '';
              return value && !value.startsWith('about:') && !value.startsWith('stcli-probe:');
            }).length,
            main_url: location.href,
            asset_natural_width: document.querySelector('img')?.naturalWidth || 0,
            cookie: cookie
          };
        })()
        """
        webView.evaluateJavaScript(script, in: nil, in: .defaultClient) { [weak self] result in
            guard let self else { return }
            let value: Any
            switch result {
            case .failure(let error):
                self.fail("observation failed: \(compactError(error))")
                return
            case .success(let observed):
                value = observed
            }
            var response: [String: Any] = [
                "id": self.request.id,
                "width": self.request.width,
                "height": height,
                "scale_factor": scale,
                "png_base64": png.base64EncodedString()
            ]
            if self.request.observe {
                var observations = value as? [String: Any] ?? [:]
                observations["denied_navigations"] = self.deniedNavigations
                observations["scheme_requests"] = self.handler.requestedURLs
                observations["webcontent_terminated"] = self.webContentTerminated
                response["observations"] = observations
            }
            self.finish(response)
        }
    }

    private func fail(_ message: String) {
        finish(["id": request.id, "error": message])
    }

    private func finish(_ response: [String: Any]) {
        guard !finished else { return }
        finished = true
        window.orderOut(nil)
        completion(response)
    }

    func webView(_ webView: WKWebView, decidePolicyFor navigationAction: WKNavigationAction, decisionHandler: @escaping (WKNavigationActionPolicy) -> Void) {
        let value = navigationAction.request.url?.absoluteString ?? ""
        if value.hasPrefix("stcli-probe://") {
            decisionHandler(.allow)
        } else {
            deniedNavigations.append(value)
            decisionHandler(.cancel)
        }
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        mainNavigationFinished = true
    }

    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
        webContentTerminated = true
        fail("webcontent process terminated")
    }
}

private final class Server {
    let fixturesDirectory: URL
    let fixtureAsset: Data?
    let ruleList: WKContentRuleList
    private var activeRenderer: Renderer?
    private var renderCount = 0
    private var watchdog: DispatchSourceTimer?
    private var idleTimer: DispatchSourceTimer?

    init(fixturesDirectory: URL, fixtureAsset: Data?, ruleList: WKContentRuleList) {
        self.fixturesDirectory = fixturesDirectory
        self.fixtureAsset = fixtureAsset
        self.ruleList = ruleList
    }

    func serve() {
        armIdleTimer()
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            while let line = readLine(strippingNewline: true) {
                DispatchQueue.main.sync { self?.handle(line) }
            }
            DispatchQueue.main.async { exit(0) }
        }
    }

    func renderForSelfTest(_ request: Request, completion: @escaping ([String: Any], ObjectIdentifier) -> Void) {
        let renderer = Renderer(request: request, ruleList: ruleList, fixtureAsset: fixtureAsset) { response in
            guard let current = self.activeRenderer else { return }
            let identifier = ObjectIdentifier(current.webView)
            self.activeRenderer = nil
            completion(response, identifier)
        }
        activeRenderer = renderer
        renderer.start()
    }

    private func handle(_ line: String) {
        idleTimer?.cancel()
        guard activeRenderer == nil else {
            emit(["error": "only one active render is allowed"])
            return
        }
        let request: Request
        do {
            request = try parseRequest(line, fixturesDirectory: fixturesDirectory)
        } catch {
            let id = ((try? JSONSerialization.jsonObject(with: Data(line.utf8)) as? [String: Any])?["id"] as? NSNumber)?.uint64Value ?? 0
            emit(["id": id, "error": (error as NSError).localizedDescription])
            armIdleTimer()
            return
        }
        armWatchdog(id: request.id, seconds: renderCount == 0 ? 10 : 3)
        let renderer = Renderer(request: request, ruleList: ruleList, fixtureAsset: fixtureAsset) { response in
            self.watchdog?.cancel()
            self.watchdog = nil
            self.activeRenderer = nil
            self.renderCount += 1
            emit(response)
            self.armIdleTimer()
        }
        activeRenderer = renderer
        renderer.start()
    }

    private func armWatchdog(id: UInt64, seconds: Int) {
        let timer = DispatchSource.makeTimerSource(queue: .main)
        timer.schedule(deadline: .now() + .seconds(seconds))
        timer.setEventHandler {
            emit(["id": id, "error": "render deadline exceeded"])
            exit(2)
        }
        timer.resume()
        watchdog = timer
    }

    private func armIdleTimer() {
        let timer = DispatchSource.makeTimerSource(queue: .main)
        timer.schedule(deadline: .now() + .seconds(30))
        timer.setEventHandler { exit(0) }
        timer.resume()
        idleTimer = timer
    }
}


private func runSelfTest(server: Server) {
    do {
        guard let encoded = argumentValue("--self-test-document-base64"), let card = Data(base64Encoded: encoded) else {
            throw NSError(domain: "probe", code: 18, userInfo: [NSLocalizedDescriptionKey: "--self-test-document-base64 requires fixture bytes"])
        }
        let first = Request(id: 1, fixture: "card", width: 800, document: card, observe: true, assets: [])
        server.renderForSelfTest(first) { response, firstView in
            guard response["error"] == nil,
                  let encoded = response["png_base64"] as? String,
                  let png = Data(base64Encoded: encoded),
                  (try? pngDimensions(png, pixelLimit: Int.max)) != nil else {
                emit(["self_test": "failed", "stage": "first_render", "response": response])
                exit(1)
            }
            let markerURL = "http://127.0.0.1:9/marker"
            let hostile = Data("<html><body><img src=\"\(markerURL)\"></body></html>".utf8)
            let second = Request(id: 2, fixture: "hostile", width: 800, document: hostile, observe: true, assets: [])
            server.renderForSelfTest(second) { secondResponse, secondView in
                let observations = secondResponse["observations"] as? [String: Any] ?? [:]
                let distinct = firstView != secondView
                let denied = (observations["asset_natural_width"] as? NSNumber)?.intValue == 0
                let passed = secondResponse["error"] == nil && distinct && denied
                emit([
                    "self_test": passed ? "ok" : "failed",
                    "png_dimensions_valid": true,
                    "webcontent_terminated": false,
                    "external_request_blocked": denied,
                    "fresh_webviews": distinct,
                    "note": "The controller-side isolation run supplies the loopback listener because the helper intentionally lacks network.server."
                ])
                exit(passed ? 0 : 1)
            }
        }
    } catch {
        emit(["self_test": "failed", "stage": "fixture_input", "error": compactError(error)])
        exit(1)
    }
}

private func argumentValue(_ name: String) -> String? {
    guard let index = CommandLine.arguments.firstIndex(of: name), index + 1 < CommandLine.arguments.count else { return nil }
    return CommandLine.arguments[index + 1]
}

private var processServer: Server?
NSApplication.shared.setActivationPolicy(.accessory)
guard let fixturesPath = argumentValue("--fixtures-dir") else {
    emit(["error": "--fixtures-dir requires a path"])
    exit(64)
}
let fixturesDirectory = URL(fileURLWithPath: fixturesPath, isDirectory: true)
let fixtureAssetURL = fixturesDirectory.appendingPathComponent("asset.png")
let fixtureAsset = try? Data(contentsOf: fixtureAssetURL)
if let fixtureAsset {
    let digest = SHA256.hash(data: fixtureAsset).map { String(format: "%02x", $0) }.joined()
    guard fixtureAsset.count <= maxAsset,
          (try? pngDimensions(fixtureAsset)) != nil,
          digest == assetDigest else {
        emit(["error": "approved asset validation failed"])
        exit(65)
    }
}
let rules = "[{\"trigger\":{\"url-filter\":\".*\"},\"action\":{\"type\":\"block\"}},{\"trigger\":{\"url-filter\":\"^stcli-probe:.*\"},\"action\":{\"type\":\"ignore-previous-rules\"}}]"
WKContentRuleListStore.default().compileContentRuleList(forIdentifier: "dev.stcli.rich-content-webkit-probe", encodedContentRuleList: rules) { ruleList, error in
    guard let ruleList else {
        emit(["error": "content rule compilation failed: \(error.map(compactError) ?? "unknown error")"])
        exit(66)
    }
    let server = Server(fixturesDirectory: fixturesDirectory, fixtureAsset: fixtureAsset, ruleList: ruleList)
    processServer = server
    if CommandLine.arguments.contains("--self-test") {
        runSelfTest(server: server)
    } else if CommandLine.arguments.contains("--worker") {
        server.serve()
    } else {
        emit(["error": "expected --worker or --self-test"])
        exit(64)
    }
}
NSApplication.shared.run()
