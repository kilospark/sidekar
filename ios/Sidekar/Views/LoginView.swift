import SwiftUI

struct LoginView: View {
    @EnvironmentObject var authService: AuthService

    var body: some View {
        VStack(spacing: 32) {
            Spacer()

            Image(systemName: "terminal")
                .font(.system(size: 56))
                .foregroundStyle(.primary)

            Text("sidekar")
                .font(.system(size: 32, weight: .bold, design: .monospaced))

            Text("Terminal access to your agent sessions")
                .font(.subheadline)
                .foregroundStyle(.secondary)

            Spacer()

            if !authService.providersLoaded {
                ProgressView()
            } else if authService.providers.isEmpty {
                Text("No login providers available")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
            } else {
                VStack(spacing: 12) {
                    ForEach(authService.providers) { provider in
                        Button(action: { authService.login(provider: provider) }) {
                            HStack(spacing: 10) {
                                providerIcon(provider.id)
                                Text("Sign in with \(provider.name)")
                            }
                            .frame(maxWidth: .infinity)
                            .padding(.vertical, 14)
                        }
                        .buttonStyle(.borderedProminent)
                        .tint(Color(white: 0.15))
                    }
                }
                .padding(.horizontal, 40)
            }

            Spacer()
                .frame(height: 60)
        }
        .preferredColorScheme(.dark)
        .task {
            await authService.fetchProviders()
        }
    }

    @ViewBuilder
    private func providerIcon(_ id: String) -> some View {
        switch id {
        case "github":
            Image(systemName: "person.crop.circle")
        case "google":
            Image(systemName: "g.circle.fill")
        default:
            Image(systemName: "arrow.right.circle")
        }
    }
}
