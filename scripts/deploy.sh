#!/bin/bash
# Usage: ./scripts/deploy.sh <user>@<host>

TARGET=$1

if [ -z "$TARGET" ]; then
    echo "Usage: $0 <user>@<host>"
    echo "Example: ./scripts/deploy.sh ec2-user@1.2.3.4"
    exit 1
fi

echo "🚀 Starting Deployment to $TARGET"

# 1. Build
echo "📦 Building release binary..."
# Note: Using standard release. If your local glibc is much newer than server,
# you might need to use 'cross' or build with musl target.
# cargo build --release --target x86_64-unknown-linux-musl
cargo build --release

if [ $? -ne 0 ]; then
    echo "❌ Build failed!"
    exit 1
fi

# 2. Deploy
echo "📤 Uploading files..."
ssh $TARGET "mkdir -p ~/funding-fee-farmer/logs"
scp target/release/funding-fee-farmer $TARGET:~/funding-fee-farmer/
scp .env $TARGET:~/funding-fee-farmer/
scp funding-fee-farmer.service $TARGET:~/funding-fee-farmer/

echo "✅ Deployment files uploaded!"
echo ""
echo "👉 NEXT STEPS (Run on server):"
echo "   sudo mv ~/funding-fee-farmer/funding-fee-farmer.service /etc/systemd/system/"
echo "   sudo systemctl daemon-reload"
echo "   sudo systemctl enable --now funding-fee-farmer"
echo "   sudo journalctl -u funding-fee-farmer -f"
