package tech.krate.player;

import android.content.Context;
import android.graphics.Canvas;
import android.graphics.Paint;
import android.graphics.Path;
import android.graphics.RectF;
import android.view.View;

/**
 * The five icons of Krate's menu, drawn as strokes so they sit with the
 * phone's own: Close (a cross), Restart (a turning arrow), Share (a box with
 * an arrow up), Access (a shield with a tick), Report (a speech bubble with
 * an exclamation mark).
 */
final class MenuIcon extends View {
    private final int which;
    private final Paint pen = new Paint(Paint.ANTI_ALIAS_FLAG);

    MenuIcon(Context context, int which, int color) {
        super(context);
        this.which = which;
        pen.setColor(color);
        pen.setStyle(Paint.Style.STROKE);
        pen.setStrokeCap(Paint.Cap.ROUND);
        pen.setStrokeJoin(Paint.Join.ROUND);
    }

    @Override
    protected void onDraw(Canvas canvas) {
        float s = Math.min(getWidth(), getHeight());
        float u = s / 24f;
        pen.setStrokeWidth(2f * u);
        canvas.save();
        canvas.translate((getWidth() - s) / 2f, (getHeight() - s) / 2f);
        Path p = new Path();
        switch (which) {
            case 0:
                p.moveTo(6 * u, 6 * u);
                p.lineTo(18 * u, 18 * u);
                p.moveTo(18 * u, 6 * u);
                p.lineTo(6 * u, 18 * u);
                break;
            case 1:
                p.addArc(new RectF(4 * u, 4 * u, 20 * u, 20 * u), -60, 300);
                p.moveTo(16 * u, 2.5f * u);
                p.lineTo(16.5f * u, 6.5f * u);
                p.lineTo(12.5f * u, 7.5f * u);
                break;
            case 2:
                p.moveTo(8 * u, 10 * u);
                p.lineTo(5 * u, 10 * u);
                p.lineTo(5 * u, 21 * u);
                p.lineTo(19 * u, 21 * u);
                p.lineTo(19 * u, 10 * u);
                p.lineTo(16 * u, 10 * u);
                p.moveTo(12 * u, 15 * u);
                p.lineTo(12 * u, 3 * u);
                p.moveTo(8.5f * u, 6.5f * u);
                p.lineTo(12 * u, 3 * u);
                p.lineTo(15.5f * u, 6.5f * u);
                break;
            case 3:
                p.moveTo(12 * u, 2.5f * u);
                p.lineTo(20 * u, 5.5f * u);
                p.cubicTo(20 * u, 13 * u, 17 * u, 18.5f * u, 12 * u, 21.5f * u);
                p.cubicTo(7 * u, 18.5f * u, 4 * u, 13 * u, 4 * u, 5.5f * u);
                p.close();
                p.moveTo(8.5f * u, 12 * u);
                p.lineTo(11 * u, 14.5f * u);
                p.lineTo(15.5f * u, 9.5f * u);
                break;
            default:
                p.addRoundRect(new RectF(3 * u, 4 * u, 21 * u, 17 * u), 4 * u, 4 * u,
                        Path.Direction.CW);
                p.moveTo(8 * u, 17 * u);
                p.lineTo(7 * u, 21 * u);
                p.lineTo(12 * u, 17 * u);
                p.moveTo(12 * u, 7.5f * u);
                p.lineTo(12 * u, 11.5f * u);
                p.moveTo(12 * u, 14 * u);
                p.lineTo(12 * u, 14.1f * u);
                break;
        }
        canvas.drawPath(p, pen);
        canvas.restore();
    }
}
