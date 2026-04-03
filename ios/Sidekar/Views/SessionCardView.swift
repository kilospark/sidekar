import SwiftUI

struct SessionCardView: View {
    let session: Session

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                Text(session.name)
                    .font(.system(size: 15, weight: .semibold, design: .monospaced))
                    .lineLimit(1)

                Text(session.agent_type.uppercased())
                    .font(.system(size: 10, weight: .medium, design: .monospaced))
                    .padding(.horizontal, 6)
                    .padding(.vertical, 2)
                    .background(Color.secondary.opacity(0.15))
                    .clipShape(RoundedRectangle(cornerRadius: 3))

                Spacer()

                Text(session.relativeTime)
                    .font(.system(size: 12, design: .monospaced))
                    .foregroundStyle(.secondary)
            }

            Text(session.hostname)
                .font(.system(size: 13, design: .monospaced))
                .foregroundStyle(.secondary)

            Text(session.cwd)
                .font(.system(size: 13, design: .monospaced))
                .foregroundStyle(.tertiary)
                .lineLimit(1)
                .truncationMode(.middle)
        }
        .padding(.vertical, 4)
    }
}
