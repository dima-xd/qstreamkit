package com.example.qprofprobe;

import android.app.Activity;
import android.app.PictureInPictureParams;
import android.content.Intent;
import android.os.Build;
import android.os.Bundle;
import android.util.Rational;
import android.view.Surface;
import android.view.SurfaceHolder;
import android.view.SurfaceView;
import android.widget.FrameLayout;

public class MainActivity extends Activity implements SurfaceHolder.Callback {
    static {
        System.loadLibrary("probe");
    }

    private SurfaceView view;
    private Thread worker;

    private native void nativeStart(Surface surface);
    private native void nativeSetPerfetto(boolean on);
    private native void nativeStop();

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        FrameLayout root = new FrameLayout(this);
        view = new SurfaceView(this);
        view.getHolder().addCallback(this);
        root.addView(view);
        setContentView(root);

        nativeSetPerfetto(getIntent().getBooleanExtra("perfetto", false));

        Intent svc = new Intent(this, StandService.class);
        if (Build.VERSION.SDK_INT >= 26) {
            startForegroundService(svc);
        } else {
            startService(svc);
        }
    }

    @Override
    public void surfaceCreated(SurfaceHolder holder) {
        final Surface surface = holder.getSurface();
        worker = new Thread(new Runnable() {
            public void run() {
                nativeStart(surface);
            }
        });
        worker.start();
    }

    @Override
    public void surfaceChanged(SurfaceHolder holder, int format, int width, int height) {
    }

    @Override
    public void surfaceDestroyed(SurfaceHolder holder) {
        nativeStop();
        if (worker != null) {
            try {
                worker.join(2000);
            } catch (InterruptedException ignored) {
            }
            worker = null;
        }
    }

    @Override
    public void onUserLeaveHint() {
        super.onUserLeaveHint();
        try {
            PictureInPictureParams params = new PictureInPictureParams.Builder()
                    .setAspectRatio(new Rational(16, 9))
                    .build();
            enterPictureInPictureMode(params);
        } catch (Exception ignored) {
        }
    }
}
