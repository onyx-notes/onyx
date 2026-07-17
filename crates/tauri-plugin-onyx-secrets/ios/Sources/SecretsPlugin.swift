// Onyx secret storage for iOS.
//
// Plain entries: Keychain items with kSecAttrAccessibleAfterFirstUnlock
// ThisDeviceOnly — never synced, never restored to another device (that
// property is what keeps the CRDT peer-id seed from being cloned by an
// OS backup restore).
//
// Protected entries: the same, plus SecAccessControl(.biometryCurrentSet):
// reads require Face ID / Touch ID, and a biometric re-enrollment
// invalidates the item — a newly enrolled face must not unlock old vault
// keys. The OS owns the prompt; there is no app-level bypass path.

import Foundation
import LocalAuthentication
import Security
import Tauri

class KeyArgs: Decodable {
    let key: String
}

class SetArgs: Decodable {
    let key: String
    let value: String
}

class ProtectedGetArgs: Decodable {
    let key: String
    var reason: String?
}

class ProtectedSetArgs: Decodable {
    let key: String
    let value: String
    var reason: String?
}

class SecretsPlugin: Plugin {
    let service = "app.onyx.secrets"
    let bioService = "app.onyx.secrets.bio"

    private func baseQuery(_ service: String, _ key: String) -> [String: Any] {
        return [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: key,
        ]
    }

    @objc public func available(_ invoke: Invoke) throws {
        let context = LAContext()
        var error: NSError?
        let biometric = context.canEvaluatePolicy(
            .deviceOwnerAuthenticationWithBiometrics, error: &error)
        invoke.resolve(["secure": true, "biometric": biometric])
    }

    @objc public func set(_ invoke: Invoke) throws {
        let args = try invoke.parseArgs(SetArgs.self)
        var query = baseQuery(service, args.key)
        SecItemDelete(query as CFDictionary)
        query[kSecValueData as String] = args.value.data(using: .utf8)!
        query[kSecAttrAccessible as String] =
            kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        let status = SecItemAdd(query as CFDictionary, nil)
        if status == errSecSuccess {
            invoke.resolve()
        } else {
            invoke.reject("keychain add failed: \(status)")
        }
    }

    @objc public func get(_ invoke: Invoke) throws {
        let args = try invoke.parseArgs(KeyArgs.self)
        var query = baseQuery(service, args.key)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        if status == errSecSuccess, let data = item as? Data {
            invoke.resolve(["value": String(data: data, encoding: .utf8)])
        } else {
            invoke.resolve(["value": nil as String?])
        }
    }

    @objc public func delete(_ invoke: Invoke) throws {
        let args = try invoke.parseArgs(KeyArgs.self)
        SecItemDelete(baseQuery(service, args.key) as CFDictionary)
        invoke.resolve()
    }

    @objc public func setProtected(_ invoke: Invoke) throws {
        let args = try invoke.parseArgs(ProtectedSetArgs.self)
        var query = baseQuery(bioService, args.key)
        SecItemDelete(query as CFDictionary)

        var error: Unmanaged<CFError>?
        guard
            let access = SecAccessControlCreateWithFlags(
                nil,
                kSecAttrAccessibleWhenUnlockedThisDeviceOnly,
                .biometryCurrentSet,
                &error)
        else {
            invoke.reject("access control unavailable")
            return
        }
        query[kSecValueData as String] = args.value.data(using: .utf8)!
        query[kSecAttrAccessControl as String] = access
        // Storing is consent: require a fresh biometric presentation.
        let context = LAContext()
        context.localizedReason = args.reason ?? "Onyx"
        query[kSecUseAuthenticationContext as String] = context
        let status = SecItemAdd(query as CFDictionary, nil)
        if status == errSecSuccess {
            invoke.resolve()
        } else {
            invoke.reject("keychain add failed: \(status)")
        }
    }

    @objc public func getProtected(_ invoke: Invoke) throws {
        let args = try invoke.parseArgs(ProtectedGetArgs.self)
        var query = baseQuery(bioService, args.key)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        let context = LAContext()
        context.localizedReason = args.reason ?? "Onyx"
        query[kSecUseAuthenticationContext as String] = context
        // Keychain drives the Face ID / Touch ID prompt itself; run off the
        // main thread so we don't deadlock the UI the prompt needs.
        DispatchQueue.global(qos: .userInitiated).async {
            var item: CFTypeRef?
            let status = SecItemCopyMatching(query as CFDictionary, &item)
            if status == errSecSuccess, let data = item as? Data {
                invoke.resolve(["value": String(data: data, encoding: .utf8)])
            } else if status == errSecItemNotFound {
                invoke.resolve(["value": nil as String?])
            } else {
                invoke.reject("biometric read failed: \(status)")
            }
        }
    }

    @objc public func deleteProtected(_ invoke: Invoke) throws {
        let args = try invoke.parseArgs(KeyArgs.self)
        SecItemDelete(baseQuery(bioService, args.key) as CFDictionary)
        invoke.resolve()
    }
}

@_cdecl("init_plugin_onyx_secrets")
func initPlugin() -> Plugin {
    return SecretsPlugin()
}
